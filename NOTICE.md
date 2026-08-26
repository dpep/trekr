# Third-party notices

trekr is MIT licensed (see [LICENSE.txt](LICENSE.txt)). It incorporates material
from the projects below.

## Rubydex

trekr's Ruby semantics are written against
[Shopify's Rubydex](https://github.com/Shopify/rubydex): its
`docs/ruby-behaviors.md` is the conformance spec, and a block of constant- and
method-resolution cases in `src/tree/mod.rs` is ported from its
`resolution_tests.rs`. trekr does not depend on the crate — the reasons are in
[docs/PLAN.md](docs/PLAN.md) §8 — but the ported cases are covered by Rubydex's
license, reproduced here in full:

```
The MIT License (MIT)

Copyright (c) 2025-present, Shopify Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## rwr

Prism node machinery and walker patterns follow [rwr](https://github.com/dpep/rwr)
(MIT), by the same author as trekr.
