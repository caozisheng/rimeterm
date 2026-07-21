--- @sync entry
--- Bridge Yazi's `hover` / `cd` DDS events into rimeterm's OSC 1337
--- channel (§5.5 of the design doc).
---
--- Why a subprocess? Yazi holds stdout for its own alt-screen renderer
--- and its Lua sandbox intercepts direct `io.stdout` writes. The
--- reliable channel is `Command`: spawn `rimectl osc-emit …` with the
--- default `Stdio.INHERIT`, so the child inherits Yazi's PTY file
--- descriptor and writes the OSC bytes straight into rimeterm's
--- non-destructive scanner. Yazi's frame never sees them (unknown
--- OSC 1337 params are silently dropped by alacritty).
---
--- Install: see `docs/yazi-setup.md` in the rimeterm repo. TL;DR:
---   1. copy this folder to `<yazi-config>/plugins/rimeterm-bridge.yazi`
---   2. append `require("rimeterm-bridge"):setup()` to
---      `<yazi-config>/init.lua`
---   3. make sure `rimectl` (bundled next to `rimeterm`) is on PATH
---
---   <yazi-config> is:
---     Windows:    %AppData%\yazi\config
---     macOS/Linux: ~/.config/yazi

local M = {}

-- Fire-and-forget: spawn `rimectl osc-emit <event> <path>`, let it exit
-- on its own. The kernel decodes the OSC envelope inside the PTY reader.
-- Any spawn error (e.g. rimectl not on PATH) goes to Yazi's log so the
-- user can `ya.dbg` diagnose without breaking navigation.
local function emit(event, url)
	if url == nil then
		return
	end
	local ok, err = pcall(function()
		Command("rimectl")
			:arg("osc-emit")
			:arg(event)
			:arg(tostring(url))
			-- INHERIT is the default; state it so the intent is obvious.
			:stdin(Command.INHERIT)
			:stdout(Command.INHERIT)
			:stderr(Command.INHERIT)
			:spawn()
	end)
	if not ok then
		ya.err("rimeterm-bridge: failed to spawn rimectl: " .. tostring(err))
	end
end

function M:setup()
	ps.sub("hover", function(body)
		if body ~= nil and body.url ~= nil then
			emit("file.selected", body.url)
		end
	end)

	ps.sub("cd", function(body)
		if body ~= nil and body.url ~= nil then
			emit("cwd.changed", body.url)
		end
	end)
end

return M
