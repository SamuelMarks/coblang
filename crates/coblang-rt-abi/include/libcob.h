/* Clean-room Apache-2.0 / MIT C-ABI header for libcoblang_rt. */
#ifndef COBLANG_RT_LIBCOB_H
#define COBLANG_RT_LIBCOB_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32)
#define COB_EXT_EXPORT __declspec(dllexport)
#define COB_EXT_IMPORT __declspec(dllimport)
#else
#define COB_EXT_EXPORT
#define COB_EXT_IMPORT extern
#endif

#define COB_TYPE_GROUP               0x01U
#define COB_TYPE_BOOL                0x02U
#define COB_TYPE_NUMERIC_DISPLAY     0x10U
#define COB_TYPE_NUMERIC_BINARY      0x11U
#define COB_TYPE_NUMERIC_PACKED      0x12U
#define COB_TYPE_NUMERIC_FLOAT       0x13U
#define COB_TYPE_NUMERIC_DOUBLE      0x14U
#define COB_TYPE_ALPHANUMERIC        0x21U
#define COB_TYPE_ALPHANUMERIC_ALL    0x22U
#define COB_TYPE_ALPHANUMERIC_EDITED 0x23U
#define COB_TYPE_NUMERIC_EDITED      0x24U
#define COB_TYPE_NATIONAL            0x40U
#define COB_TYPE_NATIONAL_EDITED     0x41U

#define COB_FLAG_HAVE_SIGN           0x01U
#define COB_FLAG_SIGN_SEPARATE       0x02U
#define COB_FLAG_SIGN_LEADING        0x04U
#define COB_FLAG_BLANK_ZERO          0x08U
#define COB_FLAG_JUSTIFIED           0x10U
#define COB_FLAG_BINARY_SWAP         0x20U
#define COB_FLAG_REAL_BINARY         0x40U
#define COB_FLAG_IS_POINTER          0x80U

#define COB_STORE_ROUND              0x01
#define COB_STORE_KEEP_ON_OVERFLOW   0x02
#define COB_STORE_TRUNC_ON_OVERFLOW  0x04

typedef struct __cob_field_attr {
    uint8_t field_type;
    uint8_t digits;
    int16_t scale;
    uint8_t flags;
    const uint8_t *pic_ptr;
} cob_field_attr;

typedef struct __cob_field {
    size_t size;
    uint8_t *data;
    const cob_field_attr *attr;
} cob_field;

typedef struct __cob_module {
    struct __cob_module *next;
    const char *module_name;
    const char *module_formatted_date;
    const char *module_source;
    const void *module_entry;
    cob_field *const *cob_procedure_params;
    int32_t module_num_params;
    uint8_t ebcdic_sign;
    uint8_t decimal_point;
    uint8_t currency_symbol;
    uint8_t numeric_separator;
    uint8_t flag_filename_mapping;
    uint8_t flag_binary_truncate;
    uint8_t flag_pretty_display;
    uint8_t flag_host_sign;
} cob_module;

typedef struct __cob_decimal {
    uint64_t limbs[4];
    int32_t sign;
    int32_t scale;
} cob_decimal;

typedef struct __cob_file {
    const char *select_name;
    uint8_t *file_status;
    cob_field *assign;
    cob_field *record;
    cob_field *variable_record;
    size_t record_min;
    size_t record_max;
    int32_t fd;
    uint8_t organization;
    uint8_t access_mode;
    uint8_t lock_mode;
    uint8_t open_mode;
    uint8_t flag_optional;
} cob_file;

void cob_init(int argc, const char *const *argv);
void cob_init_nomain(int argc, const char *const *argv);
void cob_tidy(void);
void cob_stop_run(int status);
void cob_module_enter(cob_module **module, void **cob_glob_ptr, int num_params);
void cob_module_leave(cob_module *module);
void cob_set_location(const char *sfile, uint32_t sline, const char *csect, const char *cpara, const char *cstmt);

void cob_move(const cob_field *src, cob_field *dst);
int cob_cmp(const cob_field *f1, const cob_field *f2);
int cob_numeric_cmp(const cob_field *f1, const cob_field *f2);
int cob_cmp_int(const cob_field *f, int32_t val);
int cob_cmp_uint(const cob_field *f, uint32_t val);
int32_t cob_get_int(const cob_field *f);
void cob_set_int(cob_field *f, int32_t val);
int cob_real_get_sign(cob_field *f);
void cob_real_put_sign(cob_field *f, int sign);

void cob_decimal_init(cob_decimal *d);
void cob_decimal_clear(cob_decimal *d);
void cob_decimal_set_field(cob_decimal *d, const cob_field *f);
int cob_decimal_get_field(const cob_decimal *d, cob_field *f, int opt);
void cob_decimal_set_llint(cob_decimal *d, int64_t val);
void cob_decimal_set_ullint(cob_decimal *d, uint64_t val);
void cob_decimal_add(cob_decimal *d1, const cob_decimal *d2);
void cob_decimal_sub(cob_decimal *d1, const cob_decimal *d2);
void cob_decimal_mul(cob_decimal *d1, const cob_decimal *d2);
void cob_decimal_div(cob_decimal *d1, const cob_decimal *d2);
void cob_decimal_pow(cob_decimal *d1, const cob_decimal *d2);
int cob_decimal_cmp(const cob_decimal *d1, const cob_decimal *d2);
int cob_add_int(cob_field *f, int32_t val, int opt);
int cob_sub_int(cob_field *f, int32_t val, int opt);

void cob_display(int to_stderr, int newline, int field_count, const cob_field *const *fields);
void cob_accept(cob_field *f);
void cob_accept_date(cob_field *f);
void cob_accept_date_yyyymmdd(cob_field *f);
void cob_accept_day(cob_field *f);
void cob_accept_day_yyyyddd(cob_field *f);
void cob_accept_day_of_week(cob_field *f);
void cob_accept_time(cob_field *f);
void cob_accept_arg_number(cob_field *f);
void cob_accept_arg_value(cob_field *f);
void cob_accept_command_line(cob_field *f);
void cob_accept_environment(cob_field *f);
void cob_set_environment(const cob_field *name, const cob_field *value);

void *cob_resolve(const char *name);
int cob_call_error(void);
void cob_cancel(const char *name);
int cob_call(const char *name, int argc, void *const *argv);
int cob_func(const char *name, int argc, void *const *argv);
void *cob_malloc(size_t size);
void cob_free(void *ptr);
void *cob_alloc_base(size_t size, void **baseptr);
int CBL_NOR(const uint8_t *src, uint8_t *dst, size_t length);
int C_GETPID(void);
int C_PRINTABLE(uint8_t *buf, const uint8_t *repl_ptr);

#ifdef __cplusplus
}
#endif

#endif /* COBLANG_RT_LIBCOB_H */
