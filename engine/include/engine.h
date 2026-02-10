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

#endif
