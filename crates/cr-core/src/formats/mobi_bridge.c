/* Keep libmobi's structs and memory ownership on the C side of the ABI. */
#include "mobi.h"
#include <stdlib.h>
#include <string.h>

typedef int (*emit_part)(void *, const char *, const unsigned char *, size_t);
typedef void (*emit_metadata)(void *, unsigned int, const unsigned char *, size_t);

#ifdef _WIN32
typedef wchar_t path_char;
#else
typedef char path_char;
#endif

int cr_mobi_open(const path_char *path, MOBIData **book) {
    *book = NULL;
#ifdef _WIN32
    FILE *file = _wfopen(path, L"rb");
#else
    FILE *file = fopen(path, "rb");
#endif
    if (file == NULL) {
        return MOBI_FILE_NOT_FOUND;
    }
    MOBIData *mobi = mobi_init();
    MOBI_RET status = mobi_load_file(mobi, file);
    fclose(file);
    if (status != MOBI_SUCCESS) {
        mobi_free(mobi);
        return status;
    }
    *book = mobi;
    return MOBI_SUCCESS;
}

static void send_string(void *ctx, emit_metadata emit, unsigned int tag, char *value) {
    if (value != NULL) {
        emit(ctx, tag, (const unsigned char *)value, strlen(value));
        free(value);
    }
}

void cr_mobi_metadata(const MOBIData *book, void *ctx, emit_metadata emit) {
    send_string(ctx, emit, EXTH_UPDATEDTITLE, mobi_meta_get_title(book));
    send_string(ctx, emit, EXTH_LANGUAGE, mobi_meta_get_language(book));
    send_string(ctx, emit, EXTH_ASIN, mobi_meta_get_asin(book));
    for (const MOBIExthHeader *entry = book->eh; entry != NULL; entry = entry->next) {
        switch (entry->tag) {
        case EXTH_AUTHOR:
        case EXTH_PUBLISHER:
        case EXTH_DESCRIPTION:
        case EXTH_ISBN:
        case EXTH_SUBJECT:
        case EXTH_PUBLISHINGDATE:
            send_string(ctx, emit, entry->tag,
                        mobi_decode_exthstring(book, entry->data, entry->size));
            break;
        default:
            break;
        }
    }
    MOBIExthHeader *cover = mobi_get_exthrecord_by_tag(book, EXTH_COVEROFFSET);
    size_t first = mobi_get_first_resource_record(book);
    if (cover != NULL && cover->size == 4 && first != MOBI_NOTSET) {
        uint32_t offset = mobi_decode_exthvalue(cover->data, cover->size);
        if (offset <= SIZE_MAX - first) {
            MOBIPdbRecord *record = mobi_get_record_by_seqnumber(book, first + offset);
            if (record != NULL && record->size > 0) {
                emit(ctx, EXTH_COVEROFFSET, record->data, record->size);
            }
        }
    }
}

static int send_parts(const MOBIPart *part, const char *prefix, void *ctx, emit_part emit) {
    for (; part != NULL; part = part->next) {
        if (part->size == 0) {
            continue;
        }
        MOBIFileMeta type = mobi_get_filemeta_by_type(part->type);
        char name[80];
        if (part->type == T_OPF) {
            strcpy(name, "OEBPS/content.opf");
        } else {
            snprintf(name, sizeof(name), "OEBPS/%s%05zu.%s", prefix, part->uid, type.extension);
        }
        if (emit(ctx, name, part->data, part->size) != 0) {
            return MOBI_WRITE_FAILED;
        }
    }
    return MOBI_SUCCESS;
}

int cr_mobi_read(const MOBIData *book, int epub, void *ctx, emit_part emit) {
    if (mobi_is_encrypted(book)) {
        return MOBI_FILE_ENCRYPTED;
    }
    if (mobi_is_replica(book)) {
        return MOBI_FILE_UNSUPPORTED;
    }
    MOBIRawml *raw = mobi_init_rawml(book);
    int status = mobi_parse_rawml_opt(raw, book, epub != 0, false, epub != 0);
    if (status == MOBI_SUCCESS) {
        status = send_parts(raw->markup, "part", ctx, emit);
    }
    if (status == MOBI_SUCCESS && epub && raw->flow != NULL) {
        status = send_parts(raw->flow->next, "flow", ctx, emit);
    }
    if (status == MOBI_SUCCESS && epub) {
        status = send_parts(raw->resources, "resource", ctx, emit);
    }
    mobi_free_rawml(raw);
    return status;
}
