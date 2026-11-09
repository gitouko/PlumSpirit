-- Author-written example, not a built-in algorithm generator.
plum.require_api(0, 2)

function register(p)
  p.command{
    id = "uppercase-text",
    name = "Uppercase ASCII in UTF-8 Text…",
    tooltip = "Choose an input and output; no project notes are changed.",
    run = function()
      local input = plum.files.open{
        filters = {{name = "UTF-8 text", extensions = {"txt", "csv", "json"}}}
      }
      if not input then return end
      local size = input:info().size
      local output = plum.files.save{
        name = "uppercase.txt",
        filters = {{name = "UTF-8 text", extensions = {"txt"}}}
      }
      if not output then input:close(); return end

      local carry, total = "", 0
      while true do
        local chunk = input:read(65536)
        if #chunk == 0 then break end
        total = total + #chunk
        local text = carry .. chunk
        carry = ""
        local valid, at = utf8.len(text)
        if not valid then
          -- A code point may straddle a chunk. Only its last 1–3 bytes
          -- may be deferred; invalid data is never published.
          if #text - at + 1 > 3 then error("Choose a valid UTF-8 text file.") end
          carry = text:sub(at)
          text = text:sub(1, at - 1)
          if not utf8.len(text) then error("Choose a valid UTF-8 text file.") end
        end
        output:write((text:gsub("[a-z]", function(c)
          return string.char(c:byte() - 32)
        end)))
        plum.progress(size > 0 and total / size or 1, "Transforming selected text")
      end
      if #carry > 0 then error("The selected file ends with invalid or incomplete UTF-8.") end
      input:close() -- Also allows deliberately choosing the same output file.
      output:commit() -- The explicit filesystem publication point, outside Undo.
      plum.ui.notify("Saved text with uppercase ASCII letters; UTF-8 and newlines preserved.")
    end,
  }
end
