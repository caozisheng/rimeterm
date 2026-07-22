--- @since 25.4.8

-- glow.yazi: preview Markdown files in Yazi's third column via glow(1).
--
-- rimeterm ships glow as an essential binary at `~/.rimeterm/bin/glow`.
-- Yazi's default text previewer (bat) already colours Markdown by
-- syntax, but glow renders headings / lists / code blocks / links as
-- laid-out prose, which reads much better in a narrow preview pane.
--
-- Behavior:
-- - `glow --style=auto --width=<area.w>` picks a light or dark theme
--   from the current terminal palette and reflows to the preview
--   width.
-- - Output is trimmed to the preview area height, matching Yazi's
--   built-in previewers so scroll math stays consistent.
-- - Any spawn error falls back to a plain-text message.

local M = {}

local function fail(job, msg)
	ya.preview_widget(
		job,
		ui.Text.parse(msg):area(job.area):wrap(ui.Wrap.YES)
	)
end

function M:peek(job)
	local child, err = Command("glow")
		:arg({
			"--style=auto",
			"--width=" .. tostring(job.area.w),
			tostring(job.file.url),
		})
		:stdout(Command.PIPED)
		:stderr(Command.PIPED)
		:spawn()

	if not child then
		return fail(job, "glow: spawn failed: " .. tostring(err))
	end

	local limit = job.area.h
	local i, outs, errs = 0, {}, {}
	repeat
		local next, event = child:read_line()
		if event == 1 then
			errs[#errs + 1] = next
		elseif event ~= 0 then
			break
		end

		i = i + 1
		if i > job.skip then
			outs[#outs + 1] = next
		end
	until i >= job.skip + limit

	child:start_kill()

	if #errs > 0 then
		return fail(job, "glow: " .. table.concat(errs, ""))
	end

	if job.skip > 0 and i < job.skip + limit then
		ya.emit(
			"peek",
			{ math.max(0, i - limit), only_if = job.file.url, upper_bound = true }
		)
	else
		local s = table.concat(outs, "")
		ya.preview_widget(job, ui.Text.parse(s):area(job.area))
	end
end

function M:seek(job)
	local h = cx.active.current.hovered
	if h and h.url == job.file.url then
		local step = math.floor(job.units * job.area.h / 10)
		ya.emit("peek", {
			math.max(0, cx.active.preview.skip + step),
			only_if = job.file.url,
		})
	end
end

return M
