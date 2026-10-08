# Uppsala Performance Harness

This is a local comparison harness for Uppsala's XML parser against a sibling
checkout of `libxml2`.

The harness measures parse-only time from in-memory strings. It calls libxml2
directly through `xmlReadMemory` and frees each parsed `xmlDoc`, so timings do
not include `xmllint` process startup or file I/O.

Default layout:

```text
code/
  uppsala/
    performance-harness/
  libxml2/
```

Set `LIBXML2_DIR=/path/to/libxml2` to use a different checkout.

## Build libxml2

Build a local release static library before running the harness. The default
source checkout is `../libxml2`; substitute `$LIBXML2_DIR` when using a custom
checkout:

```bash
cmake -S ../libxml2 -B ../libxml2/build-uppsala-release -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=OFF \
  -DLIBXML2_WITH_PROGRAMS=OFF \
  -DLIBXML2_WITH_TESTS=OFF \
  -DLIBXML2_WITH_ZLIB=OFF \
  -DLIBXML2_WITH_ICONV=OFF \
  -DLIBXML2_WITH_ICU=OFF \
  -DLIBXML2_WITH_MODULES=OFF \
  -DLIBXML2_WITH_PYTHON=OFF \
  -DCMAKE_C_FLAGS_RELEASE='-O3 -DNDEBUG -fno-semantic-interposition -march=native'
cmake --build ../libxml2/build-uppsala-release
```

The harness build script looks for
`${LIBXML2_DIR:-../libxml2}/build-uppsala-release/libxml2.a` by default. To use
another build directory:

```bash
LIBXML2_LIB_DIR=/path/to/libxml2/build cargo run --release \
  --manifest-path performance-harness/Cargo.toml -- saml 101
```

## One-Command Report

From the repository root:

```bash
just bench-libxml2
```

This builds libxml2 and the native Uppsala harness, then prints one final table.
Uppsala is measured through the stable DOM API, namespace-disabled DOM API,
pull scan-only API, and explicit pull-to-DOM API, alongside libxml2. The report
includes:

- SAML-shaped response documents
- a larger generated SAML metadata aggregate
- a larger generated Atom feed archive
- a larger generated SOAP invoice batch
- a local pyFF metadata fixture
- larger XML fixtures from the libxml2 checkout

Use `LIBXML2_DIR=/path/to/libxml2 just bench-libxml2` for a non-default
checkout.

## Run SAML-Shaped Inputs

```bash
RUSTFLAGS='-C target-cpu=native' cargo run --release \
  --manifest-path performance-harness/Cargo.toml -- saml 101
```

This generates three namespace-heavy SAML-like responses in memory:

- `saml-small`, about 3.5 KB
- `saml-medium`, about 9 KB
- `saml-large`, about 28 KB

These are synthetic parser fixtures, not security or interoperability test
vectors.

## Run One File

```bash
RUSTFLAGS='-C target-cpu=native' cargo run --release \
  --manifest-path performance-harness/Cargo.toml -- \
  file test-data/pyff-xslt/sample-metadata.xml 101
```

The final argument is the sample count. If omitted, it defaults to `31`.

## Run A Fixed File Suite

```bash
RUSTFLAGS='-C target-cpu=native' cargo run --release \
  --manifest-path performance-harness/Cargo.toml -- \
  suite /path/to/benchmark-files 101
```

The suite directory must contain:

- `fonts.conf`
- `medium.svg`
- `large.plist`
- `huge.xml`
- `gigantic.svg`
- `cdata.xml`
- `text.xml`
- `attributes.xml`

## Output

Output is tab-separated:

```text
file  bytes  uppsala_ns_us  uppsala_no_ns_us  uppsala_pull_scan_us  uppsala_pull_dom_us  libxml2_us  ratio_ns  ratio_no_ns  ratio_pull_scan  ratio_pull_dom
```

The `Ratio` columns are `libxml2 / Uppsala`; values above `1.0` mean Uppsala
parsed faster. `uppsala_ns_us` is the namespace-aware DOM parser,
`uppsala_no_ns_us` disables namespace resolution, `uppsala_pull_scan_us` drains
the pull event stream without materializing a DOM, and `uppsala_pull_dom_us`
builds a DOM from the pull event stream via `document_from_pull`. That last
column is the explicit event-to-DOM path kept for differential testing; it is
expected to be slower than `uppsala_ns_us`, which builds the arena directly
from the shared tokenizer (ADR 0019).

On x86_64, this harness exercises Uppsala's SSE2 scanners. For server
deployment benchmarking, prefer `RUSTFLAGS='-C target-cpu=native'` so LLVM can
use the host CPU's scheduling model and baseline extensions where applicable.
