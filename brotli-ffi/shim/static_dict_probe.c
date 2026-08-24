/* A window onto one encoder-internal function, for differential testing.
 *
 * `BrotliFindAllStaticDictionaryMatches` is `BROTLI_INTERNAL`: it has no
 * public header, and its `BrotliEncoderDictionary` argument is only reachable
 * through a shared-dictionary structure the public API never exposes. This
 * shim builds that structure once and forwards the call, so the Rust port of
 * the search can be compared against the reference it was translated from.
 *
 * Nothing outside the test suite links this. It lives here rather than in
 * `vendor/`, which is upstream source and is not hand-edited.
 */

#include "enc/encoder_dict.h"
#include "enc/static_dict.h"

int mbrotli_shim_find_all_static_dictionary_matches(const uint8_t* data,
                                                    size_t min_length,
                                                    size_t max_length,
                                                    uint32_t* matches) {
  /* Built on the stack every call rather than cached in a static: the test
     harness runs tests on several threads, and a lazily initialised static
     would be a data race. `BrotliInitSharedEncoderDictionary` only assigns
     fields — the word tables it points at are initialised elsewhere — so this
     costs nothing worth caching. */
  SharedEncoderDictionary shared;
  BrotliInitSharedEncoderDictionary(&shared);
  return BrotliFindAllStaticDictionaryMatches(
      shared.contextual.dict[0], data, min_length, max_length, matches);
}

/* A window onto the high-quality block splitter, for differential testing.
 *
 * `BrotliSplitBlock` is `BROTLI_INTERNAL` and takes a `MemoryManager` and a
 * `BrotliEncoderParams` the public API never exposes. This shim builds both,
 * runs the splitter over a caller-supplied command array, and copies the three
 * partitions into plain output buffers.
 *
 * The `Command` layout is part of the contract: the Rust side passes its own
 * command array straight through, which is only sound because the two structs
 * agree field for field.
 */

#include <string.h>

#include "enc/block_splitter.h"
#include "enc/command.h"
#include "enc/memory.h"
#include "enc/params.h"
#include "enc/quality.h"

/* Copies one partition out, up to |capacity| blocks. Returns its block count. */
static size_t mbrotli_shim_copy_split(const BlockSplit* split, size_t capacity,
                                      uint8_t* types, uint32_t* lengths,
                                      size_t* num_types) {
  size_t i;
  size_t n = split->num_blocks < capacity ? split->num_blocks : capacity;
  *num_types = split->num_types;
  for (i = 0; i < n; ++i) {
    types[i] = split->types[i];
    lengths[i] = split->lengths[i];
  }
  return split->num_blocks;
}

void mbrotli_shim_split_block(int quality, int lgwin, const Command* commands,
                              size_t num_commands, const uint8_t* data,
                              size_t pos, size_t mask, size_t capacity,
                              uint8_t* literal_types, uint32_t* literal_lengths,
                              size_t* literal_num_types, size_t* literal_blocks,
                              uint8_t* command_types, uint32_t* command_lengths,
                              size_t* command_num_types, size_t* command_blocks,
                              uint8_t* distance_types,
                              uint32_t* distance_lengths,
                              size_t* distance_num_types,
                              size_t* distance_blocks) {
  MemoryManager m;
  BrotliEncoderParams params;
  BlockSplit literal_split;
  BlockSplit command_split;
  BlockSplit distance_split;

  BrotliInitMemoryManager(&m, 0, 0, 0);
  /* `BrotliSplitBlock` reads only the quality, which sets the refinement
     iteration count; zeroing the rest keeps the shim independent of the
     static initialiser the encoder keeps to itself. */
  memset(&params, 0, sizeof(params));
  params.quality = quality;
  params.lgwin = lgwin;

  BrotliInitBlockSplit(&literal_split);
  BrotliInitBlockSplit(&command_split);
  BrotliInitBlockSplit(&distance_split);

  BrotliSplitBlock(&m, commands, num_commands, data, pos, mask, &params,
                   &literal_split, &command_split, &distance_split);

  *literal_blocks = mbrotli_shim_copy_split(&literal_split, capacity,
                                            literal_types, literal_lengths,
                                            literal_num_types);
  *command_blocks = mbrotli_shim_copy_split(&command_split, capacity,
                                            command_types, command_lengths,
                                            command_num_types);
  *distance_blocks = mbrotli_shim_copy_split(&distance_split, capacity,
                                             distance_types, distance_lengths,
                                             distance_num_types);

  BrotliDestroyBlockSplit(&m, &literal_split);
  BrotliDestroyBlockSplit(&m, &command_split);
  BrotliDestroyBlockSplit(&m, &distance_split);
}

/* A window onto the high-quality meta-block builder, for differential testing.
 *
 * `BrotliBuildMetaBlock` chooses the distance alphabet, splits the three symbol
 * streams and clusters their histograms into the context maps the decoder
 * reads. None of that is reachable through the public API, and all of it is
 * decided before a single bit is written, so comparing it directly is the only
 * way to tell a difference in the builder from a difference in the search that
 * fed it.
 *
 * The command array is rewritten in place, exactly as the encoder does when the
 * distance alphabet changes.
 */

#include "enc/metablock.h"

void mbrotli_shim_build_meta_block(
    int quality, int lgwin, int context_mode, int disable_literal_context_modeling,
    uint8_t* data, size_t pos, size_t mask, uint8_t prev_byte, uint8_t prev_byte2,
    Command* commands, size_t num_commands, size_t capacity,
    uint32_t* out_npostfix, uint32_t* out_ndirect,
    size_t* literal_num_types, size_t* command_num_types, size_t* distance_num_types,
    size_t* literal_histograms, size_t* command_histograms, size_t* distance_histograms,
    uint32_t* literal_context_map, size_t* literal_context_map_size,
    uint32_t* distance_context_map, size_t* distance_context_map_size) {
  MemoryManager m;
  BrotliEncoderParams params;
  MetaBlockSplit mb;
  size_t i;

  BrotliInitMemoryManager(&m, 0, 0, 0);
  memset(&params, 0, sizeof(params));
  params.quality = quality;
  params.lgwin = lgwin;
  params.disable_literal_context_modeling =
      TO_BROTLI_BOOL(disable_literal_context_modeling);
  BrotliInitDistanceParams(&params.dist, 0, 0, BROTLI_FALSE);

  InitMetaBlockSplit(&mb);
  BrotliBuildMetaBlock(&m, data, pos, mask, &params, prev_byte, prev_byte2,
                       commands, num_commands, (ContextType)context_mode, &mb);

  *out_npostfix = params.dist.distance_postfix_bits;
  *out_ndirect = params.dist.num_direct_distance_codes;
  *literal_num_types = mb.literal_split.num_types;
  *command_num_types = mb.command_split.num_types;
  *distance_num_types = mb.distance_split.num_types;
  *literal_histograms = mb.literal_histograms_size;
  *command_histograms = mb.command_histograms_size;
  *distance_histograms = mb.distance_histograms_size;

  *literal_context_map_size = mb.literal_context_map_size;
  for (i = 0; i < mb.literal_context_map_size && i < capacity; ++i) {
    literal_context_map[i] = mb.literal_context_map[i];
  }
  *distance_context_map_size = mb.distance_context_map_size;
  for (i = 0; i < mb.distance_context_map_size && i < capacity; ++i) {
    distance_context_map[i] = mb.distance_context_map[i];
  }

  DestroyMetaBlockSplit(&m, &mb);
}

/* A window onto the Zopfli backward-reference search, for differential testing.
 *
 * Qualities ten and eleven choose their commands by dynamic programming over
 * every match the binary-tree hasher can find. Comparing the resulting command
 * stream directly is the only way to separate a difference in that search from
 * a difference in the meta-block builder that consumes it.
 *
 * The hasher is set up exactly as `EncodeData` sets it up for a first block.
 */

#include "enc/backward_references_hq.h"
#include "enc/hash.h"
#include "../common/context.h"

size_t mbrotli_shim_zopfli_references(int quality, int lgwin, const uint8_t* ringbuffer,
                                      size_t ringbuffer_mask, size_t position,
                                      size_t num_bytes, int* dist_cache,
                                      size_t* last_insert_len, size_t* num_literals,
                                      Command* commands, size_t capacity) {
  MemoryManager m;
  BrotliEncoderParams params;
  Hasher hasher;
  size_t num_commands = 0;
  ContextLut lut = BROTLI_CONTEXT_LUT(CONTEXT_UTF8);

  BrotliInitMemoryManager(&m, 0, 0, 0);
  memset(&params, 0, sizeof(params));
  params.quality = quality;
  params.lgwin = lgwin;
  params.stream_offset = 0;
  BrotliInitDistanceParams(&params.dist, 0, 0, BROTLI_FALSE);
  BrotliInitSharedEncoderDictionary(&params.dictionary);
  ChooseHasher(&params, &params.hasher);

  HasherInit(&hasher);
  InitOrStitchToPreviousBlock(&m, &hasher, ringbuffer, ringbuffer_mask, &params,
                              position, num_bytes, BROTLI_TRUE);

  if (quality == 10) {
    BrotliCreateZopfliBackwardReferences(&m, num_bytes, position, ringbuffer,
                                         ringbuffer_mask, lut, &params, &hasher,
                                         dist_cache, last_insert_len, commands,
                                         &num_commands, num_literals);
  } else {
    BrotliCreateHqZopfliBackwardReferences(&m, num_bytes, position, ringbuffer,
                                           ringbuffer_mask, lut, &params, &hasher,
                                           dist_cache, last_insert_len, commands,
                                           &num_commands, num_literals);
  }

  DestroyHasher(&m, &hasher);
  BrotliCleanupSharedEncoderDictionary(&m, &params.dictionary);
  (void)capacity;
  return num_commands;
}
