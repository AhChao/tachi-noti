#!/bin/bash
# Claude Code status line: current model, context window fill, and 5-hour / 7-day plan usage.
# Chained after tachi-noti in ~/.claude/settings.json (symlink ~/.claude/statusline/context-usage.sh here):
#   "command": "tachi-noti statusline --chain ~/.claude/statusline/context-usage.sh"
# Reads only the JSON Claude Code passes on stdin (no API calls); one jq call per refresh.
# Colours: under 30% green, 30-60% yellow, over 60% red (a starting point, tune to taste).

jq -r '
  def pct: if . == null then null else (. | floor) end;
  def colour(p): if p == null then "\u001b[2m"
                 elif p >= 60 then "\u001b[31m"
                 elif p >= 30 then "\u001b[33m"
                 else "\u001b[32m" end;
  def human: if . >= 1000000 then "\((. / 100000 | floor) / 10)M"
             elif . >= 1000 then "\(. / 1000 | floor)k"
             else "\(.)" end;
  def reset_in(ts): if ts == null then "" else
      ((ts - now) / 60 | floor) as $m
      | if $m < 0 then "" elif $m >= 60 then " \($m / 60 | floor)h\($m % 60)m" else " \($m)m" end
    end;

  (.model.display_name // .model.id // null) as $model
  | (.context_window.used_percentage | pct) as $ctx
  | (.context_window.context_window_size // null) as $size
  | ((.context_window.current_usage // null)
      | if . == null then null
        else (.input_tokens // 0) + (.cache_creation_input_tokens // 0) + (.cache_read_input_tokens // 0) end) as $used
  | (.rate_limits.five_hour.used_percentage | pct) as $h5
  | (.rate_limits.seven_day.used_percentage | pct) as $d7
  | [
      ( if $model == null then empty else "\u001b[36m\($model)\u001b[0m" end ),
      ( colour($ctx) + "ctx "
        + (if $ctx == null then "--" else "\($ctx)%" end)
        + (if $used != null and $size != null then " (\($used | human)/\($size | human))" else "" end)
        + "\u001b[0m" ),
      ( if $h5 == null then empty
        else colour($h5) + "5h \($h5)%"
             + (if $h5 >= 60 then reset_in(.rate_limits.five_hour.resets_at) else "" end)
             + "\u001b[0m" end ),
      ( if $d7 == null then empty
        else colour($d7) + "7d \($d7)%" + "\u001b[0m" end )
    ]
  | join(" · ")
' 2>/dev/null || printf 'ctx --'
