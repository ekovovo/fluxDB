This directory is intentionally empty.

`rust_i18n::i18n!` only needs a valid glob target. Runtime translations are
loaded from `assets/locales` and the profile's `locales` directory so adding a
language does not require changing Rust code or rebuilding the application.
