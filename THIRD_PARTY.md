# Third-party notices

Vampir itself is MIT-licensed (see [LICENSE](LICENSE)). It is built on, and its example application ships with, the software below. Each is under its own licence, reproduced or linked here as those licences require.

## GPUI Community Edition

Vampir's `gpui` dependency is [`gpui-ce`](https://github.com/gpui-ce/gpui-ce), the community edition of GPUI, together with its platform crate `gpui_ce_platform`. GPUI is written by Zed Industries and the community edition tracks it, so the copyright is theirs:

> Copyright 2022 - 2025 Zed Industries, Inc.
>
> Licensed under the Apache License, Version 2.0 (the "License"); you may not use this file except in compliance with the License. You may obtain a copy of the License at
>
> http://www.apache.org/licenses/LICENSE-2.0
>
> Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.

The full text of the Apache License, Version 2.0 is at <https://www.apache.org/licenses/LICENSE-2.0.txt> and ships inside the crates as `LICENSE-APACHE`. Vampir uses gpui-ce as published to crates.io and carries no modified copy of it.

## Everything else

The remaining dependencies (`unicode-segmentation`, and for the `app-icon` feature `image` and the `objc2` crates) are MIT or Apache-2.0 dual-licensed; `cargo tree` lists them and `cargo license` prints each one's terms. The gallery's icon and the other files under `assets/` are part of this repository and MIT-licensed with it.
