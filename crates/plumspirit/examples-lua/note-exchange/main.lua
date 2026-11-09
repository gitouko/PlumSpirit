-- The plugin authors this text format; PlumSpirit supplies streams and transactions.
local CHUNK, BATCH, MAX_LINE, MAX_NOTES = 16384, 4096, 128, 1000000
local function target(ctx)
  return assert(ctx.project:track(1), "Note Exchange needs a first Track.")
end
local function import_notes(ctx)
  local writer=ctx.tx:note_writer(target(ctx))
  local batch={start={},gate={},key={},velocity={}}
  local line_number,count=0,0
  local function flush()
    if #batch.start>0 then writer:append(batch);batch={start={},gate={},key={},velocity={}} end
  end
  local function line(value)
    line_number=line_number+1
    assert(#value<=MAX_LINE,"Note Exchange line exceeds128bytes.")
    value=value:gsub("\r$", "")
    if line_number==1 then assert(value=="PNOTES1","Expected PNOTES1 header.");return end
    if value=="" then return end
    local a,b,c,d=value:match("^(%d+),(%d+),(%d+),(%d+)$")
    assert(a,"Invalid four-column note at line "..line_number..".")
    a,b,c,d=tonumber(a),tonumber(b),tonumber(c),tonumber(d)
    for _,v in ipairs({a,b,c,d}) do assert(math.type(v)=="integer","Integer overflow at line "..line_number..".") end
    assert(a>=0 and b>=0 and c>=0 and c<ctx.keys and d>=1 and d<=127,"Invalid note values at line "..line_number..".")
    count=count+1;assert(count<=MAX_NOTES,"Note Exchange demonstration cap is one million notes.")
    local n=#batch.start+1
    batch.start[n]=a;batch.gate[n]=b;batch.key[n]=c;batch.velocity[n]=d
    if n==BATCH then flush() end
  end
  local carry=""
  local size=ctx.file:info().size
  while true do
    local chunk=ctx.file:read(CHUNK)
    if #chunk==0 then break end
    local text=carry..chunk;local first=1
    while true do
      local finish=text:find("\n",first,true)
      if not finish then break end
      line(text:sub(first,finish-1));first=finish+1
    end
    carry=text:sub(first);assert(#carry<=MAX_LINE,"Note Exchange line exceeds128bytes.")
    plum.progress(size>0 and ctx.file:info().offset/size or 1,"Decoding Note Exchange")
  end
  if #carry>0 then line(carry) end
  assert(line_number>0,"Note Exchange file is empty.")
  flush();writer:finish()
end
local function export_notes(ctx)
  local notes=target(ctx):notes()
  local count=notes:count()
  assert(count<=MAX_NOTES,"Note Exchange demonstration cap is one million notes.")
  ctx.file:write("PNOTES1\n")
  local written=0
  for batch in notes:batches{size=BATCH,columns={"start","gate","key","velocity"}} do
    local lines={}
    for i=1,batch.count do
      lines[i]=string.format("%d,%d,%d,%d\n",batch.start[i],batch.gate[i],batch.key[i],batch.velocity[i])
    end
    ctx.file:write(table.concat(lines));written=written+batch.count
    plum.progress(count>0 and written/count or 1,"Encoding Note Exchange")
  end
end
function register(p)
  p.format{id="notes",name="Note Exchange (four columns)",extensions={"pnotes"},import=import_notes,export=export_notes}
end
