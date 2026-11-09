use super::*;
use crate::lua::{Host, Limits, Plain};
use unicode_segmentation::UnicodeSegmentation;

fn run(source: &str) -> Host {
    let host = Host::new(Limits::default(), []).unwrap();
    assert_eq!(
        host.run("unicode.lua", source).unwrap(),
        Plain::Boolean(true)
    );
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    host
}

#[test]
fn unicode_symbols_joined_emoji_and_exact_byte_ranges_reach_lua() {
    run(r#"
        plum.require_api(0,2)
        local expected={'∑','∞','π','♫','♬','★','◆','♥','😀','🔥','👍🏽','👨‍👩‍👧‍👦','🇯🇵','🇺🇸','1️⃣','é','✈️','\r\n','\0','日'}
        local original=table.concat(expected)
        assert(plum.text.unicode_version=='17.0.0')
        assert(plum.text.grapheme_count(original)==#expected)
        local pieces={};local previous=1;local count=0
        for cluster,first,after,index in plum.text.graphemes(original) do
            count=count+1
            assert(index==count and cluster==expected[count])
            assert(first==previous and original:sub(first,after-1)==cluster)
            assert(after-first==#cluster)
            previous=after;pieces[#pieces+1]=cluster
        end
        assert(count==#expected and previous==#original+1 and table.concat(pieces)==original)
        assert(plum.text.grapheme_count('')==0)
        assert(plum.text.graphemes('')()==nil)
        assert(plum.text.grapheme_count('é')==1 and plum.text.grapheme_count('é')==1)
        assert(plum.text.graphemes('é')()=='é' and plum.text.graphemes('é')()=='é')
        assert(plum.text.grapheme_count('👨‍👩‍👧‍👦')==1)
        return true
    "#);
}

#[test]
fn chunked_cursor_matches_whole_unicode_boundaries_across_scripts_and_splits() {
    let cases = [
        "a\r\nb\0c".to_string(),
        "👍🏽👨‍👩‍👧‍👦🇯🇵🇺🇸1️⃣e\u{301}✈️".repeat(5),
        "हिन्दी नमस्ते क्ष क्\u{200d}ष বাংলা العربية עברית".to_string(),
        "각 한글 日本語 a\u{301}\u{308}\u{332}".to_string(),
        format!(
            "{}👨‍👩‍👧‍👦{}🇯🇵",
            "x".repeat(CHUNK_BYTES - 1),
            "e\u{301}".repeat(13)
        ),
        format!("a{}b", "\u{301}".repeat(5000)),
    ];
    for text in cases {
        let expected = text
            .grapheme_indices(true)
            .map(|(start, s)| (start, start + s.len()))
            .collect::<Vec<_>>();
        for chunk in (1..=33).chain([CHUNK_BYTES - 1, CHUNK_BYTES, CHUNK_BYTES + 1]) {
            let mut scan = Scanner::new(&text, chunk);
            let mut got = Vec::new();
            while let Some((start, end, index)) = scan.next(&text, || Ok(())).unwrap() {
                assert_eq!(index, got.len() + 1);
                got.push((start, end));
            }
            assert_eq!(got, expected, "chunk={chunk}");
        }
    }
}

#[test]
fn invalid_utf8_types_and_text_cluster_count_limits_fail_without_replacement() {
    run(r#"
        for _,value in ipairs({123,true,{},string.char(255),string.char(0xC0,0x80),string.char(0xED,0xA0,0x80)}) do
            assert(not pcall(plum.text.grapheme_count,value))
            assert(not pcall(plum.text.graphemes,value))
        end
        assert(not pcall(plum.text.grapheme_count,string.rep('a',plum.text.max_bytes+1)))
        assert(plum.text.grapheme_count(string.rep('a',plum.text.max_graphemes))==plum.text.max_graphemes)
        assert(not pcall(plum.text.grapheme_count,string.rep('a',plum.text.max_graphemes+1)))
        local one='é'..string.rep(utf8.char(0x301),(plum.text.max_cluster_bytes-2)//2)
        assert(#one==plum.text.max_cluster_bytes and plum.text.grapheme_count(one)==1)
        local bad=plum.text.graphemes(one..utf8.char(0x301))
        assert(not pcall(bad))
        assert(bad()==nil) -- a failed cursor is closed rather than resumed mid-cluster
        return true
    "#);
}

#[test]
fn iterators_expire_release_native_state_on_eof_error_and_job_completion() {
    let host = Host::new(Limits::default(), []).unwrap();
    host.lua
        .globals()
        .set(
            "native_bytes",
            host.lua
                .create_function(|lua, ()| Ok(lua.app_data_ref::<NativeBudget>().unwrap().used()))
                .unwrap(),
        )
        .unwrap();
    host.run(
        "text-scope.lua",
        r#"
        kept=plum.text.graphemes('👍🏽')
        assert(native_bytes()>0 and kept()=='👍🏽')
        assert(kept()==nil and native_bytes()==0 and kept()==nil)
        kept=plum.text.graphemes('hello')
        assert(native_bytes()>0)
    "#,
    )
    .unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    assert_eq!(
        host.run("expired-text.lua", "kept()").unwrap_err().category,
        Category::Expired
    );
    let error = host
        .run(
            "text-failure.lua",
            "kept=plum.text.graphemes('abc');error('deliberate text failure')",
        )
        .unwrap_err();
    assert!(error.message.contains("deliberate text failure"));
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    let error = host
        .run(
            "iterator-count.lua",
            "local held={} for n=1,65 do held[n]=plum.text.graphemes('a') end",
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    host.lua.set_app_data(NativeBudget::isolated(0));
    assert_eq!(
        host.run("scalar-count.lua", "return plum.text.grapheme_count('é')")
            .unwrap(),
        Plain::Integer(1)
    );
    assert_eq!(
        host.run("native-text-budget.lua", "plum.text.graphemes('a')")
            .unwrap_err()
            .category,
        Category::Budget
    );
}

#[test]
fn cancellation_occurs_inside_a_cluster_and_caught_worker_cancel_releases_state() {
    let text = format!("a{}", "\u{301}".repeat(5000));
    let mut scanner = Scanner::new(&text, 512);
    let mut checks = 0;
    let error = scanner
        .next(&text, || {
            checks += 1;
            if checks == 3 {
                Err(stop(Category::Canceled, "controlled Unicode cancellation"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(checks, 3);
    assert!(scanner.cursor.cur_cursor() > 0 && scanner.count == 0);
    assert!(super::super::error_text(&error).contains("controlled Unicode cancellation"));
    let host = Host::new(Limits::default(), []).unwrap();
    let cancel = host.cancel_handle();
    host.lua
        .globals()
        .set(
            "cancel_now",
            host.lua
                .create_function(move |_, ()| {
                    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    let error = host
        .run(
            "cancel-text.lua",
            "kept=plum.text.graphemes('abc');cancel_now();kept()",
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Canceled);
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn streamed_graphemes_use_bounded_native_state_under_small_vm_quota() {
    let host = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    let budget = NativeBudget::isolated(4096);
    host.lua.set_app_data(budget.clone());
    assert_eq!(
        host.run(
            "large-unicode.lua",
            r#"
        local cluster='a'..string.rep(utf8.char(0x301),7)
        local input=string.rep(cluster,40000)
        local total,count=0,0
        for value,first,after,index in plum.text.graphemes(input) do
            assert(value==cluster and first==total+1 and after==total+#cluster+1 and index==count+1)
            total=total+#value;count=count+1
        end
        assert(total==#input and count==40000)
        return count
    "#
        )
        .unwrap(),
        Plain::Integer(40000)
    );
    assert_eq!(budget.used(), 0);
}
