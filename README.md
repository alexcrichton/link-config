# link-config

A syntax extension for Rust that runs `pkg-config` at build-time to figure how
how to link native dependencies.

```rust
#![feature(phase)]

#[phase(plugin)]
extern crate link_config = "link-config";

link_config!("libcurl")

extern {
    fn curl_easy_init() -> *mut ();
}

fn main() {
    let handle = unsafe { curl_easy_init() };
    // ...
}
```

## Dynamic vs Static linking

An invocation of the `link_config!` macro will generate two extern blocks that
look like:

```rust
// foo.rs
link_config!("mylib")

// foo-expanded.rs
#[cfg(statik = "mylib")]
#[link(..., kind = "static")]
extern {}

#[cfg(not(statik = "mylib"))]
#[link(...)]
extern {}
```

This means that a dynamic dependency is the default, but a static dependency can
be specified via:

```
$ rustc foo.rs --cfg 'statik="mylib"'
```

## Configuring emission

The full syntax for an invocation is currently:

```rust
link_config!("foo", ["bar", "baz"])
```

The library being linked is called `foo` and both `bar`/`baz` are options to the
`link_config!` macro itself. The currently known options are:

* `only_static` - Only emit a block for a static linkage, and enable it by
                  default.
* `only_dylib` - Only emit a block for a dynamic linkage, and enable it by
                 default.
* `favor_static` - Instead of emiting `not(statik = "mylib")`, emit
                   `not(dylib = "mylib")`, favoring the static block by default.
* `system_static` - Allow system dependencies to be statically linked. This is
                    not allowed by default.

## How does it work?

When linking native libraries, this syntax extension is interested in answering
three questions:

* What is the local name of the native library?
* What are the dependencies of the native library?
* Where is everything located?

This library is *not* interested in various platform-specific flags to the
linker and other various configuration options that are not always necessary.

To answer these questions, this library currently shells out to `pkg-config` at
*build time* with the `--libs` option and filters the return value to answer the
questions above. For static linking the tool is invoked with `--static`

The syntax extension then generates an `extern` block with appropriate `#[link]`
and `#[cfg]` attributes.

## TODO list

* Custom rust script to have platform-specific logic for determining libraries
  and dependencies. This will also be useful for tools that don't necessarily
  use `pkg-config` like LLVM or postgres.
* Integrate [`pkgconf`](https://github.com/pkgconf/pkgconf) as a fallback if
  `pkg-config` is not available.


# License

Serde is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Serde by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
