# atrac9dec (vendored)

[atrac9dec](https://github.com/claimore22/atrac9dec) 0.9.1, MIT, a Rust port of
[LibAtrac9](https://github.com/Thealexbarney/LibAtrac9) (MIT, Alex Barney).

Changed here to match LibAtrac9, so low-bitrate game music (DOOM, Astroneer) decodes:

- Band extension is read and applied (`src/bex.rs`) instead of skipped.
- Joint-stereo signs are read from the stereo start unit, not unit 0.
- Intensity stereo runs before spectrum scaling.
