#ifndef LEX_ENGINE_H
#define LEX_ENGINE_H

#include <stdint.h>

const char * _Nonnull lex_engine_version(void);
int32_t lex_engine_echo(int32_t x);

/* Tracing API (no-op unless built with --features trace) */
void lex_trace_init(const char * _Nonnull log_dir);

/* Dictionary API */

typedef struct LexDict LexDict;              /* Rust: TrieDictionary */

typedef struct {
    const char * _Nonnull reading;
    const char * _Nonnull surface;
    int16_t cost;
} LexCandidate;

typedef struct {
    const LexCandidate * _Nullable candidates;
    uint32_t len;
    void * _Nullable _owned;
} LexCandidateList;

LexDict * _Nullable lex_dict_open(const char * _Nonnull path);
void lex_dict_close(LexDict * _Nullable dict);
LexCandidateList lex_dict_lookup(const LexDict * _Nonnull dict, const char * _Nonnull reading);
LexCandidateList lex_dict_predict(const LexDict * _Nonnull dict, const char * _Nonnull prefix, uint32_t max_results);
void lex_candidates_free(LexCandidateList list);

/* Connection matrix API */

typedef struct LexConnectionMatrix LexConnectionMatrix;  /* Rust: ConnectionMatrix */

LexConnectionMatrix * _Nullable lex_conn_open(const char * _Nonnull path);
void lex_conn_close(LexConnectionMatrix * _Nullable conn);

/* Conversion API (lattice + Viterbi) */

typedef struct {
    const char * _Nonnull reading;
    const char * _Nonnull surface;
} LexSegment;

typedef struct {
    const LexSegment * _Nullable segments;
    uint32_t len;
    void * _Nullable _owned;
} LexConversionResult;

LexConversionResult lex_convert(
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const char * _Nonnull kana
);
void lex_conversion_free(LexConversionResult result);

/* User History API */

typedef struct LexUserHistory LexUserHistory;  /* Rust: LexUserHistoryWrapper (RwLock<UserHistory>) */

LexUserHistory * _Nullable lex_history_open(const char * _Nonnull path);
void lex_history_close(LexUserHistory * _Nullable history);
void lex_history_record(const LexUserHistory * _Nonnull history, const LexSegment * _Nonnull segments, uint32_t len);
int32_t lex_history_save(const LexUserHistory * _Nonnull history, const char * _Nonnull path);

LexConversionResult lex_convert_with_history(
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const LexUserHistory * _Nonnull history,
    const char * _Nonnull kana
);

LexCandidateList lex_dict_lookup_with_history(
    const LexDict * _Nonnull dict,
    const LexUserHistory * _Nonnull history,
    const char * _Nonnull reading
);

LexCandidateList lex_dict_predict_ranked(
    const LexDict * _Nonnull dict,
    const LexUserHistory * _Nullable history,
    const char * _Nonnull prefix,
    uint32_t max_results
);

/* N-best Conversion API */

typedef struct {
    const LexConversionResult * _Nullable results;
    uint32_t len;
    void * _Nullable _owned;
} LexConversionResultList;

LexConversionResultList lex_convert_nbest(
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const char * _Nonnull kana,
    uint32_t n
);

LexConversionResultList lex_convert_nbest_with_history(
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const LexUserHistory * _Nonnull history,
    const char * _Nonnull kana,
    uint32_t n
);

void lex_conversion_result_list_free(LexConversionResultList list);

/* Romaji Lookup API */

typedef struct {
    uint8_t tag;           /* 0=none, 1=prefix, 2=exact, 3=exactAndPrefix */
    const char * _Nullable kana;      /* valid when tag=2 or tag=3; NULL otherwise */
    void * _Nullable _owned;
} LexRomajiLookupResult;

LexRomajiLookupResult lex_romaji_lookup(const char * _Nonnull romaji);
void lex_romaji_lookup_free(LexRomajiLookupResult result);

/* Romaji Convert API */

typedef struct {
    const char * _Nullable composed_kana;
    const char * _Nullable pending_romaji;
    void * _Nullable _owned;
} LexRomajiConvertResult;

LexRomajiConvertResult lex_romaji_convert(
    const char * _Nonnull composed_kana,
    const char * _Nonnull pending_romaji,
    uint8_t force
);
void lex_romaji_convert_free(LexRomajiConvertResult result);

/* Unified Candidate Generation API */

typedef struct {
    const char * _Nullable const * _Nullable surfaces;
    uint32_t surfaces_len;
    const LexConversionResult * _Nullable paths;
    uint32_t paths_len;
    void * _Nullable _owned;
} LexCandidateResponse;

LexCandidateResponse lex_generate_candidates(
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const LexUserHistory * _Nullable history,
    const char * _Nonnull reading,
    uint32_t max_results
);
LexCandidateResponse lex_generate_prediction_candidates(
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const LexUserHistory * _Nullable history,
    const char * _Nonnull reading,
    uint32_t max_results
);
void lex_candidate_response_free(LexCandidateResponse response);

/* InputSession API */

typedef struct LexSession LexSession;  /* Rust: LexSession (wraps InputSession) */

typedef struct {
    uint8_t consumed;
    const char * _Nullable commit_text;       /* NULL = no commit */
    const char * _Nullable marked_text;       /* NULL = no change, "" = clear */
    uint8_t is_dashed_underline;   /* 1 = English submode underline */
    const char * _Nullable const * _Nullable candidates;
    uint32_t candidates_len;
    uint32_t selected_index;
    uint8_t show_candidates;       /* 1 = show/update candidate panel */
    uint8_t hide_candidates;       /* 1 = hide candidate panel */
    uint8_t switch_to_abc;         /* 1 = switch to ABC input source */
    uint8_t save_history;          /* 1 = trigger async history save */
    uint8_t needs_candidates;      /* 1 = caller should generate candidates async */
    const char * _Nullable candidate_reading; /* reading for async generation (valid when needs_candidates=1) */
    uint8_t candidate_dispatch;    /* 0=standard, 1=prediction, 2=neural */
    const char * _Nullable ghost_text;        /* NULL=no change, ""=clear, string=show */
    uint8_t needs_ghost_text;      /* 1 = caller should generate ghost text async */
    const char * _Nullable ghost_context;     /* context for ghost generation (valid when needs_ghost_text=1) */
    uint64_t ghost_generation;     /* staleness counter */
    void * _Nullable _owned;
} LexKeyResponse;

LexSession * _Nullable lex_session_new(
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const LexUserHistory * _Nullable history
);
void lex_session_free(LexSession * _Nullable session);
void lex_session_set_programmer_mode(LexSession * _Nonnull session, uint8_t enabled);
void lex_session_set_defer_candidates(LexSession * _Nonnull session, uint8_t enabled);
void lex_session_set_conversion_mode(LexSession * _Nonnull session, uint8_t mode);

LexKeyResponse lex_session_handle_key(
    LexSession * _Nonnull session,
    uint16_t key_code,
    const char * _Nullable text,
    uint8_t flags  /* bit0=shift, bit1=has_modifier(Cmd/Ctrl/Opt) */
);

LexKeyResponse lex_session_commit(LexSession * _Nonnull session);
uint8_t lex_session_is_composing(const LexSession * _Nonnull session);

/* Get the committed context string for neural candidate generation.
 * Returns NULL if the context is empty.
 * Caller must free with lex_committed_context_free. */
char * _Nullable lex_session_committed_context(const LexSession * _Nonnull session);
void lex_committed_context_free(char * _Nullable ptr);

void lex_key_response_free(LexKeyResponse response);
uint32_t lex_key_response_history_count(const LexKeyResponse * _Nonnull response);

/* Receive async candidate results and update session state.
 * reading: the kana used for generation (staleness check).
 * Returns a LexKeyResponse with updated marked text and candidates. */
LexKeyResponse lex_session_receive_candidates(
    LexSession * _Nonnull session,
    const char * _Nullable reading,
    const LexCandidateResponse * _Nonnull candidates
);

/* Record history entries from a key response into the user history.
 * Call this before lex_key_response_free when save_history is set. */
void lex_key_response_record_history(
    const LexKeyResponse * _Nonnull response,
    const LexUserHistory * _Nonnull history
);

/* Ghost text session API */
LexKeyResponse lex_session_receive_ghost_text(
    LexSession * _Nonnull session,
    uint64_t generation,
    const char * _Nonnull text
);
uint64_t lex_session_ghost_generation(const LexSession * _Nonnull session);

/* Neural scorer API */

typedef struct LexNeuralScorer LexNeuralScorer;  /* Rust: LexNeuralScorer (Mutex<NeuralScorer>) */

LexNeuralScorer * _Nullable lex_neural_open(const char * _Nonnull model_path);
void lex_neural_close(LexNeuralScorer * _Nullable scorer);

typedef struct {
    const char * _Nullable text;
    void * _Nullable _owned;
} LexGhostTextResult;

LexGhostTextResult lex_neural_generate_ghost(
    LexNeuralScorer * _Nonnull scorer,
    const char * _Nullable context,
    uint32_t max_tokens
);
void lex_ghost_text_free(LexGhostTextResult result);

LexCandidateResponse lex_generate_neural_candidates(
    LexNeuralScorer * _Nonnull scorer,
    const LexDict * _Nonnull dict,
    const LexConnectionMatrix * _Nullable conn,
    const LexUserHistory * _Nullable history,
    const char * _Nullable context,
    const char * _Nullable reading,
    uint32_t max_results
);

#endif
