# License

`videodrome` is licensed under either of

- **[Apache License, Version 2.0](LICENSE-APACHE)** (SPDX: `Apache-2.0`)
- **[MIT license](LICENSE-MIT)** (SPDX: `MIT`)

at your option.

## Rationale

Dual `MIT OR Apache-2.0` is the [convention](https://rust-lang.github.io/api-guidelines/necessities.html#c-permissive)
in the Rust ecosystem. Consumers pick whichever fits their project — MIT
if they want the shortest, most permissive text; Apache-2.0 if they need
the explicit patent grant.

## Contributing

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.

## Third-party notices

`videodrome` bundles or depends on:

- **[librqbit](https://github.com/ikatson/rqbit)** — BitTorrent client library. Apache-2.0.
- **[hls.js](https://github.com/video-dev/hls.js)** — HLS player for MSE-based browsers. Apache-2.0.
- **[Tauri](https://tauri.app/)** — desktop app runtime. Apache-2.0 / MIT.
- **[ffmpeg](https://ffmpeg.org/)** — invoked as external process (not bundled). LGPL-2.1+ / GPL as configured by the distribution.
- **[React](https://react.dev/)** — UI library. MIT.
- Icons: **[Phosphor Icons](https://phosphoricons.com/)** — MIT.

Third-party metadata providers used at runtime (TMDB, OpenSubtitles,
Metahub, Torrentio) are consumed via their public APIs under their own
terms of service. `videodrome` does not redistribute their data.

## No warranty

The software is provided "AS IS", without warranty of any kind. It is
your responsibility to comply with the copyright laws of your
jurisdiction when using it to access media content.
