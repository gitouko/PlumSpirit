plum.require_api(0,2)
local observed = 0
local latest
plum.plugin.observe{
  id="transport", events={"transport_changed"}, debounce_ms=100,
  run=function(event)
    local snapshot=plum.playback.snapshot()
    assert(snapshot and snapshot.tick==event.data.tick)
    observed=observed+1
    latest=snapshot
    print(string.format("TRANSPORT %s tick=%d bpm=%.6f speed=%.6f count=%d",
      snapshot.state,snapshot.tick,snapshot.bpm,snapshot.speed,observed))
  end
}
plum.plugin.command{
  id="snapshot", name="Capture Transport Snapshot",
  run=function()
    local snapshot=plum.playback.snapshot()
    if not snapshot then plum.ui.notify("No captured project transport is available.","warning"); return end
    -- This is the author's own preset, saved only on an explicit command.
    plum.presets.save("Last transport snapshot",{version=1,snapshot=snapshot,
      observed=observed,latest_observed=latest})
    local text=string.format("Transport %s at tick %d; %.6f BPM; %.3fx; %d observed updates.",
      snapshot.state,snapshot.tick,snapshot.bpm,snapshot.speed,observed)
    print(text);plum.ui.notify(text)
  end
}
