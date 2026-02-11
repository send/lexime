#ifndef LEX_ENGINE_H
#define LEX_ENGINE_H

#include <stdint.h>

const char *lex_engine_version(void);
int32_t lex_engine_echo(int32_t x);

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

#endif
