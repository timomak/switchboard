# Third-party notices

This is a fork of [akitaonrails/ai-usagebar](https://github.com/akitaonrails/ai-usagebar),
based on commit `ade569256fd20053a67b50336e8b203f5481b5eb`. Its MIT license
is retained in [LICENSE](LICENSE).

The Codex account-switching implementation in `src/codex_account/` adapts the
storage, isolated login, app lifecycle and transactional switching designs from
[liuzhao1225/codex-account-switcher](https://github.com/liuzhao1225/codex-account-switcher),
commit `ea05123f98c872bf9e122b6d907e42874c116e06`, particularly
`AccountStore.swift`, `CodexClient.swift`, `DesktopController.swift`, and
`SwitchService.swift`. The implementation was ported to Rust to keep credential
handling in ai-usagebar's existing core. The upstream license follows:

MIT License

Copyright (c) 2026 liuzhao1225

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
