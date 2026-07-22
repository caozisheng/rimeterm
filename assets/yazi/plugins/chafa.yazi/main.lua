--- @since 25.4.8

-- chafa.yazi: preview images in Yazi's third column via chafa(1).
--
-- Used when the host terminal does not support Sixel / Kitty / iTerm2
-- image protocols (e.g. plain Windows Terminal). rimeterm ships chafa
-- as an essential binary at `~/.rimeterm/bin/chafa` so this plugin
-- always resolves via the augmented PATH.
--
-- Behavior:
-- - `chafa --format=symbols` renders coloured Unicode block art that
--   works over any UTF-8 terminal; no terminal-specific graphics
--   protocol required.
-- - Output is trimmed to the preview area height so tall images don't
--   overwhelm the pane.
-- - Any spawn or decode failure falls back to a plain-text message so
--   Yazi never renders an empty third column.

local M = {}

local function fail(job, msg)
	ya.preview_widget(
		job,
		ui.Text.parse(msg):area(job.area):wrap(ui.Wrap.YES)
	)
end

function M:peek(job)
	local child, err = Command("chafa")
		:arg({
			"--format=symbols",
			"--colors=full",
			"--size=" .. tostring(job.area.w) .. "x" .. tostring(job.area.h),
			"--polite=on",
			"--animate=off",
			"--passthrough=none",
			tostring(job.file.url),
		})
		:stdout(Command.PIPED)
		:stderr(Command.PIPED)
		:spawn()

	if not child then
		return fail(job, "chafa: spawn failed: " .. tostring(err))
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
		return fail(job, "chafa: " .. table.concat(errs, ""))
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
