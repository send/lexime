#ifndef LEX_ENGINE_H
#define LEX_ENGINE_H

#include <stdint.h>

const char *lex_engine_version(void);
int32_t lex_engine_echo(int32_t x);

/* Tracing API (no-op unless built with --features trace) */
void lex_trace_init(const char *log_dir);

/* Dictionary API */

typedef struct LexDict LexDict;

typedef struct {
    const char *reading;
    const char *surface;
    int16_t cost;
} LexCandidate;

typedef struct {
    const LexCandidate *candidates;
    uint32_t len;
    void *_owned;
} LexCandidateList;

LexDict *lex_dict_open(const char *path);
void lex_dict_close(LexDict *dict);
LexCandidateList lex_dict_lookup(const LexDict *dict, const char *reading);
LexCandidateList lex_dict_predict(const LexDict *dict, const char *prefix, uint32_t max_results);
void lex_candidates_free(LexCandidateList list);

/* Connection matrix API */

typedef struct LexConnectionMatrix LexConnectionMatrix;

LexConnectionMatrix *lex_conn_open(const char *path);
void lex_conn_close(LexConnectionMatrix *conn);

/* Conversion API (lattice + Viterbi) */

typedef struct {
    const char *reading;
    const char *surface;
} LexSegment;

typedef struct {
    const LexSegment *segments;
    uint32_t len;
    void *_owned;
} LexConversionResult;

LexConversionResult lex_convert(
    const LexDict *dict,
    const LexConnectionMatrix *conn,
    const char *kana
);
void lex_conversion_free(LexConversionResult result);

/* User History API */

typedef struct LexUserHistory LexUserHistory;

LexUserHistory *lex_history_open(const char *path);
void lex_history_close(LexUserHistory *history);
void lex_history_record(const LexUserHistory *history, const LexSegment *segments, uint32_t len);
int32_t lex_history_save(const LexUserHistory *history, const char *path);

LexConversionResult lex_convert_with_history(
    const LexDict *dict,
    const LexConnectionMatrix *conn,
    const LexUserHistory *history,
    const char *kana
);

LexCandidateList lex_dict_lookup_with_history(
    const LexDict *dict,
    const LexUserHistory *history,
    const char *reading
);

LexCandidateList lex_dict_predict_ranked(
    const LexDict *dict,
    const LexUserHistory *history,
    const char *prefix,
    uint32_t max_results
);

/* N-best Conversion API */

typedef struct {
    const LexConversionResult *results;
    uint32_t len;
    void *_owned;
} LexConversionResultList;

LexConversionResultList lex_convert_nbest(
    const LexDict *dict,
    const LexConnectionMatrix *conn,
    const char *kana,
    uint32_t n
);

LexConversionResultList lex_convert_nbest_with_history(
    const LexDict *dict,
    const LexConnectionMatrix *conn,
    const LexUserHistory *history,
    const char *kana,
    uint32_t n
);

void lex_conversion_result_list_free(LexConversionResultList list);

/* Romaji Lookup API */

typedef struct {
    uint8_t tag;           /* 0=none, 1=prefix, 2=exact, 3=exactAndPrefix */
    const char *kana;      /* valid when tag=2 or tag=3; NULL otherwise */
    void *_owned;
} LexRomajiLookupResult;

LexRomajiLookupResult lex_romaji_lookup(const char *romaji);
void lex_romaji_lookup_free(LexRomajiLookupResult result);

/* Romaji Convert API */

typedef struct {
    const char *composed_kana;
    const char *pending_romaji;
    void *_owned;
} LexRomajiConvertResult;

LexRomajiConvertResult lex_romaji_convert(
    const char *composed_kana,
    const char *pending_romaji,
    uint8_t force
);
void lex_romaji_convert_free(LexRomajiConvertResult result);

/* Unified Candidate Generation API */

typedef struct {
    const char *const *surfaces;
    uint32_t surfaces_len;
    const LexConversionResult *paths;
    uint32_t paths_len;
    void *_owned;
} LexCandidateResponse;

LexCandidateResponse lex_generate_candidates(
    const LexDict *dict,
    const LexConnectionMatrix *conn,
    const LexUserHistory *history,
    const char *reading,
    uint32_t max_results
);
void lex_candidate_response_free(LexCandidateResponse response);

/* InputSession API */

typedef struct LexSession LexSession;

typedef struct {
    uint8_t consumed;
    const char *commit_text;       /* NULL = no commit */
    const char *marked_text;       /* NULL = no change, "" = clear */
    uint8_t is_dashed_underline;   /* 1 = English submode underline */
    const char *const *candidates;
    uint32_t candidates_len;
    uint32_t selected_index;
    uint8_t show_candidates;       /* 1 = show/update candidate panel */
    uint8_t hide_candidates;       /* 1 = hide candidate panel */
    uint8_t switch_to_abc;         /* 1 = switch to ABC input source */
    uint8_t save_history;          /* 1 = trigger async history save */
    uint8_t needs_candidates;      /* 1 = caller should generate candidates async */
    const char *candidate_reading; /* reading for async generation (valid when needs_candidates=1) */
    void *_owned;
} LexKeyResponse;

LexSession *lex_session_new(
    const LexDict *dict,
    const LexConnectionMatrix *conn,
    const LexUserHistory *history
);
void lex_session_free(LexSession *session);
void lex_session_set_programmer_mode(LexSession *session, uint8_t enabled);
void lex_session_set_defer_candidates(LexSession *session, uint8_t enabled);

LexKeyResponse lex_session_handle_key(
    LexSession *session,
    uint16_t key_code,
    const char *text,
    uint8_t flags  /* bit0=shift, bit1=has_modifier(Cmd/Ctrl/Opt) */
);

LexKeyResponse lex_session_commit(LexSession *session);
uint8_t lex_session_is_composing(const LexSession *session);

void lex_key_response_free(LexKeyResponse response);

/* Receive async candidate results and update session state.
 * reading: the kana used for generation (staleness check).
 * Returns a LexKeyResponse with updated marked text and candidates. */
LexKeyResponse lex_session_receive_candidates(
    LexSession *session,
    const char *reading,
    const LexCandidateResponse *candidates
);

/* Record history entries from a key response into the user history.
 * Call this before lex_key_response_free when save_history is set. */
void lex_key_response_record_history(
    const LexKeyResponse *response,
    const LexUserHistory *history
);

#endif
