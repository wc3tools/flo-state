# flo-state

Lightweight actor library inspired by [actix](https://docs.rs/actix) and [xactor](https://docs.rs/xactor).

See `tests/integration_test.rs` for usage example.

## Why

The lifecycle of `Addr<T>` of other actor frameworks is equivalent to `Arc<T>`, which doesn't work well with `RAII`.

`flo-state` distinguishes between `Owner<T>` and `Addr<T>`, and when `Owner<T>` is dropped, all related `Addr<T>`s and the associated spawned tasks are cancelled deterministically.