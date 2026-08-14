# nxr-sdk-java

Java bindings for [NX Rates](https://nxrates.com): MITCH ticker resolution,
fixed-width record codecs, and synth composition, bound directly to the Rust
core.

The binding uses the **FFM API** (`java.lang.foreign`, stable since Java 22),
not JNI. The Rust side at [`../ffi`](../ffi) exports a `#[no_mangle] extern "C"`
surface as a `cdylib`; Java binds it with `Linker` / `SymbolLookup` /
`MethodHandle`. No C glue, no `native` methods, no generated headers, and the
struct layouts are declared once per side and size-asserted on both.

## Requirements

- JDK 22 or newer (26 tested)
- Gradle 9 or newer, and a Rust toolchain on `PATH`

No Gradle wrapper is committed: the wrapper jar is a build artifact. Use a
system Gradle.

## Build and test

```sh
gradle test     # builds the Rust cdylib, compiles, runs JUnit
gradle build    # + jar
```

`cargoBuild` runs `cargo build --release` in `../ffi` and the `test` task points
the binding at the resulting library via `-Dnxr.sdk.lib`. At runtime outside
Gradle, set `-Dnxr.sdk.lib=/path/to/libnxr_sdk_ffi.{dylib,so}` or `$NXR_SDK_LIB`;
with neither, the usual `java.library.path` search applies. Callers must pass
`--enable-native-access=ALL-UNNAMED`.

## Surface

| Method | Notes |
| --- | --- |
| `Nxr.tryResolveTickerId(String)` | `OptionalLong`, empty when unresolvable. **Prefer this.** |
| `Nxr.resolveTickerId(String)` | Lenient. See the phantom caveat below. |
| `Nxr.resolveTicker(long)` | `Ticker(base, quote, instrumentType)` |
| `Nxr.getMarketProvider(int)` | `Optional<MarketProvider>` |
| `Nxr.decodeIdxBytes(byte[])` | `List<IndexRecord>`, 56 B records |
| `Nxr.decodeBarBytes(byte[])` | `List<Bar>`, 96 B records |
| `Nxr.decodeTickBytes(byte[])` | `List<Tick>`, 32 B records |
| `Nxr.encodeIdxRecord(IndexRecord)` | 56 wire bytes |
| `Nxr.encodeBar(Bar)` | 96 wire bytes, lossless |
| `Nxr.computeSynthTick(List<SynthLeg>)` | `Optional<SynthTick>` |
| `Nxr.liveStringCount()` | Outstanding native strings; 0 at rest |

```java
long id = Nxr.tryResolveTickerId("BTC/USDT").orElseThrow();
Nxr.Ticker t = Nxr.resolveTicker(id);             // BITCOIN / TETHER / SPOT
List<Nxr.IndexRecord> recs = Nxr.decodeIdxBytes(body);  // from GET /v1/idx/{sym}
```

`resolveTickerId` is lenient: an unresolvable symbol yields an FNV1a-64
*phantom* id rather than an error. It is unique but not a bit-packed ticker id,
so `resolveTicker` reverses it to a hex base with an empty quote and its class
bits are hash noise. This is the same caveat the Python binding carries. Reach
for `tryResolveTickerId`.

`resolveTicker` returns asset **names**, not symbols: `BTC/USDT` reverses to
`BITCOIN` / `TETHER`.

## Memory

Owned strings cross the boundary as `*mut c_char` and are released by
`nxr_string_free` in a `finally` before the call returns, so no public method
can leak on any path. `Nxr.liveStringCount()` exposes the native
handed-out-minus-freed counter; the suite asserts it is 0 after 200k round
trips, and a companion test proves the counter is not vacuously 0 by holding
1000 strings un-freed and watching it rise.

Byte buffers are length-checked in Rust before any read. A buffer that is empty,
or whose length is not a whole multiple of the record size, is rejected with
`NXR_ERR_BUF_LEN` rather than read past. This matches the check the core applies
to every fixed-width slab (`core/src/server/signed.rs::decode_blob`,
`sdk/rust/src/bar_reader.rs`).

## Deliberate omissions

Two parts of the Python binding are intentionally absent.

**NumPy dtype helpers** (`index_record_dtype` and friends) have no Java analogue.
They exist so NumPy can reinterpret a buffer zero-copy; Java's equivalent is the
`MemoryLayout` already declared in `NxrNative`, and the decoders return typed
records directly.

**`MulticastSubscriber` and the HTTP `Client`.** Java's standard library already
covers both: `java.net.MulticastSocket` / `DatagramChannel` for UDP multicast and
`java.net.http.HttpClient` for REST and WebSocket. Wrapping the Rust
implementations would add an FFI boundary, a second async runtime inside the JVM
process, and thread-affinity constraints, in exchange for transport code the
platform already ships. A thin Java implementation on top of the decoders above
is strictly better here. The decoders are the part worth binding, because the
wire format is where a reimplementation would silently drift.

## License

MIT, see [`../LICENSE`](../LICENSE).
