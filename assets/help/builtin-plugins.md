# Built-in Plugins

Five plugins are part of PlumSpirit (nothing is downloaded). Open them from the **Plugins**
menu. Each window shows its original author and license (also in {cmd:ShowAbout}), previews
before anything changes, applies as one Undo step and remembers its settings.

* **Spiderweb** — draw note art from shapes: lines, curves, circles, polygons and stars,
  text in any installed font, with outline / fill, velocity ramps and channel rules. Paste
  into the Piano Roll as new subtracks or subchannels. The board shows the song's playhead:
  click or drag its ruler to move it (hold Shift to snap). Its transport buttons and
  {cmd:PlayPause} work the main transport, so you hear the song from where you are drawing.
  Moving the playhead is never an Undo step. *Original by Kanade Tachibana (MIT),
  expanded for PlumSpirit by ouko.*
* **Image to MIDI** — open a picture (PNG, JPEG, BMP, GIF, WebP, or drop it on the window):
  it is fitted to a key range, its colors reduced to a palette, and each color becomes a
  subtrack or a subchannel. *After ImageToMidi by arduano.*
* **HZ-Chop** — chops each selected note into short notes whose length follows the
  frequency of its key. *Original by Softy107, rewritten natively.*
* **SAFOR — Simple And Fast Overlaps Remover** — removes doubled notes on the same tick and
  key, cuts sustains under overlaps, and removes quiet notes, for the selection, a track,
  the selected tracks or the whole project. *After DixelU's SAFOR.*
* **Audio Reference** — plays an audio file with the song for orientation (WAV, MP3, OGG,
  FLAC), aligned by its start; its path is saved with the project.

Big jobs run in the background with a Stop / Cancel button. For community plugins written
in Lua see [Lua Plugins and Custom Tools](#lua-plugins).
