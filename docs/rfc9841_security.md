# RFC 9841 security notes

What an attacker can influence through the RFC 9841 features this crate
implements, and what the implementation does about it. Sections about features
that are not implemented say so rather than describing protections that do not
exist.

## Implemented: the declared window

A caller may declare any window in `10..=62`. RFC 9841 allows it, and the
number is written into the stream header verbatim.

**The risk.** A naive encoder sizes its history to the declared window. At 62
bits that is four exbibytes for a stream that may hold four bytes — a
denial-of-service reachable from one parameter.

**What is done.** The declared window and the retained history are separate
numbers. `ResolvedWindow::encoder_bits` caps retained history at 30 bits, and
everything that costs memory — the ring buffer, the block size, the match
finders — is sized from *that*. Declaring 62 bits allocates exactly as much as
declaring 30. The ring buffer additionally grows with the input rather than to
its nominal size.

**What a caller must still not do.** Nothing: no window in the legal range is
dangerous, and no illegal one can be built. `WindowBits` wraps a private enum,
so `WindowBits::standard` and `WindowBits::large` are the only constructors and
each rejects its own out-of-range values before a value exists at all.

## Implemented: integer safety

The window arithmetic that RFC 9841 widens is bounded rather than checked at
run time, and the bound is the point:

- retained history is at most 30 bits, so `1 << encoder_bits()` and the largest
  backward distance `2^encoder_bits() - 16` are provably inside a `usize` on
  every supported target;
- the usable distance alphabet stops at `BROTLI_MAX_ALLOWED_DISTANCE`
  (`(1 << 31) - 4`), which is the value chosen so that a 32-bit decoder can do
  distance arithmetic without overflow;
- `distance_code_limit` operates on `u32` values that are all below `2^31`, and
  its one subtraction that could underflow (`distance_bits - 1`) is guarded by
  a proof, stated in a comment, that `offset >= 4` forces `distance_bits >= 1`;
- the widest alphabet any legal `(NPOSTFIX, NDIRECT)` pair can produce is
  exactly 544 symbols, which is the size of the distance histogram — asserted
  by a unit test over every legal pair rather than assumed.

No compression path in `src/` contains `unsafe`. The only `unsafe` blocks in
the tree are inside `#[cfg(test)]` modules, where they call the pinned C
reference through the FFI oracle; nothing they touch ships.

## Implemented: what a large window does not change

A large window changes the header and the distance alphabet. It does not change
what is compressed, how literals are modelled, or what a decoder does with the
output. It carries no dictionary, no external reference and no caller-supplied
identifier, so it introduces no new trust boundary.

## Implemented: resource limits on a shared context

A `SharedContext` can be built from LZ77 prefix dictionaries, and dictionary
bytes usually arrive from somewhere less trusted than the code that compresses
with them. `SharedContextLimits` bounds three things, all checked by
`SharedContextBuilder::prepare` before the first table is allocated:

| Limit | Default | Refusal |
| --- | --- | --- |
| `max_total_source_bytes` | 64 MiB | `DictionaryTooLarge` |
| `max_prefix_bytes` | 64 MiB | `DictionaryTooLarge` |
| `max_allocated_bytes` (peak) | 1 GiB | `SharedContextTooLarge` |

Two properties matter more than the numbers:

- **The allocation check runs on a prediction of the *peak*, not on the
  result.** Building an index holds its scratch tables and the finished ones at
  once, roughly eight bytes per source byte, and the peak is what an attacker
  would aim at. The bound is computed from the attachment sizes before anything
  is allocated, so a context that would exceed the limit is refused rather than
  allocated and then thrown away.
- **Every check runs before the first allocation, and construction is
  all-or-nothing.** A refused build leaves no partial context, no temporary
  buffer and no cache entry, and there is no global registry that a repeated
  attempt could grow.

Two further bounds are structural rather than configurable: at most fifteen
attachments (`TooManyPrefixDictionaries`, the RFC's own limit), and at most
`2^31 - 1` bytes per attachment (`DictionaryTooLarge`). The second closes a
truncation the reference leaves open — `CreatePreparedDictionary` casts
`source_size` to a `uint32_t` without checking, so a 4 GiB+ dictionary there
would index its own head; this port refuses the segment instead.

Nothing in the dictionary path indexes without a bounds check, and no
arithmetic on a length, an offset or a distance wraps: the addressing functions
use checked `u64` throughout and return `None` rather than a wrapped value at
either end of the prefix.

## Not implemented: dictionary trust

Serialized shared dictionaries are not implemented, and no encoder consults an
attached prefix dictionary yet. When they do, the following will apply and this
section will be rewritten as behaviour rather than intent:

- **Dictionary bytes deserve the same suspicion as decompressed content.**
  Changing one byte of a dictionary changes what a stream decodes to. A decoder
  that attaches the wrong dictionary gets wrong plaintext, not an error.
- **CRIME-style side channels.** Compressing attacker-controlled data together
  with secret data leaks the secret through the compressed size, and a
  caller-chosen dictionary makes that leak *steerable*: an attacker who
  supplies the dictionary can probe secret input one guess at a time. A public
  dictionary does not make the payloads safe to mix. Nothing this crate emits
  today is affected, because a non-empty context is refused rather than used —
  but `Compressor::longest_prefix_match` already reports how much of an input a
  dictionary covers, which is the same information a size oracle leaks. Do not
  expose it to a caller who does not already own the dictionary and the input.
- **Resource exhaustion from parsed structure.** Serialized dictionaries carry
  lengths and counts read from bytes. Every one of them has to be checked
  against a configured limit before anything is allocated for it; the two
  limits reserved for that work — `max_transformed_word_bytes` and
  `max_trie_nodes` — land with the parser rather than ahead of it.

## Not implemented: container identifiers

The framing container is not implemented. When it is, note that RFC 9841's
256-bit HighwayHash identifier is a **lookup identifier, not authentication**:
the RFC does not pin a key, an input domain or a lane order, so the value
cannot be computed interoperably without one, and it would not be a
cryptographic integrity check even if it could. Anything adversarial needs a
separate trusted transport or a real MAC.

Likewise, a container's `id` metadata field is stored and returned verbatim.
It is not a path, and this crate will not interpret it as one; an extractor
that writes files must do its own path-traversal checking.
