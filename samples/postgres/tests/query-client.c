#define _POSIX_C_SOURCE 200809L
#include <libpq-fe.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static PGconn *connection;
static FILE *output;
static unsigned counter;
static const char *checksum;

_Noreturn static void fail(const char *message)
{
    fprintf(stderr, "query-client: %s\n", message);
    if (connection) {
        fprintf(stderr, "%s", PQerrorMessage(connection));
        PQfinish(connection);
    }
    exit(EXIT_FAILURE);
}

static uint64_t now_ns(void)
{
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0)
        fail("clock_gettime failed");
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec;
}

static PGresult *query(const char *sql)
{
    PGresult *result = PQexec(connection, sql);
    if (!result || (PQresultStatus(result) != PGRES_TUPLES_OK &&
                    PQresultStatus(result) != PGRES_COMMAND_OK)) {
        if (result)
            fprintf(stderr, "%s", PQresultErrorMessage(result));
        fail("SQL failed");
    }
    return result;
}

static void expect(PGresult *result, int columns, const char *const *values)
{
    if (PQresultStatus(result) != PGRES_TUPLES_OK || PQntuples(result) != 1 ||
        PQnfields(result) != columns)
        fail("unexpected result shape");
    for (int column = 0; column < columns; ++column) {
        if (PQgetisnull(result, 0, column) ||
            strcmp(PQgetvalue(result, 0, column), values[column]) != 0) {
            fprintf(stderr, "column %d: received \"%s\", expected \"%s\"\n",
                    column, PQgetvalue(result, 0, column), values[column]);
            fail("SQL result mismatch");
        }
    }
    PQclear(result);
}

static void emit(const char *phase, const char *workload, unsigned index, uint64_t elapsed)
{
    if (elapsed == 0 || fprintf(output, "%s,%s,%u,%llu\n", phase, workload, index,
                                (unsigned long long)elapsed) < 0)
        fail("invalid timing or CSV write failed");
}

static void run(const char *workload, unsigned warmup, unsigned count)
{
    uint64_t samples[1000];
    if (count > 1000)
        fail("sample count exceeds bound");
    uint64_t batch_start = 0;
    for (unsigned i = 0; i < warmup + count; ++i) {
        char sql[256];
        char number[32];
        const char *values[3];
        int columns = 1;
        const char *statement = sql;
        if (strcmp(workload, "select") == 0) {
            statement = "SELECT 1;";
            values[0] = "1";
        } else if (strcmp(workload, "lookup") == 0) {
            unsigned id = (i * 7919U) % 100000U + 1U;
            snprintf(number, sizeof(number), "%u", id);
            snprintf(sql, sizeof(sql),
                     "SELECT id,length(value),value=repeat(md5(id::text),4) "
                     "FROM spill_fixture WHERE id=%u;", id);
            values[0] = number;
            values[1] = "128";
            values[2] = "t";
            columns = 3;
        } else if (strcmp(workload, "commit") == 0) {
            statement = "UPDATE eval_commits SET counter=counter+1 WHERE id=1 RETURNING counter;";
            snprintf(number, sizeof(number), "%u", ++counter);
            values[0] = number;
        } else if (strcmp(workload, "spill") == 0) {
            statement = "SELECT md5(string_agg(id::text || ':' || value,E'\\n')) FROM "
                        "(SELECT id,value FROM spill_fixture ORDER BY value COLLATE \"C\",id) ordered;";
            values[0] = checksum;
        } else {
            fail("unknown workload");
        }
        if (i == warmup)
            batch_start = now_ns();
        uint64_t start = now_ns();
        PGresult *result = query(statement);
        uint64_t elapsed = now_ns() - start;
        expect(result, columns, values);
        if (i >= warmup)
            samples[i - warmup] = elapsed;
    }
    uint64_t batch = now_ns() - batch_start;
    for (unsigned i = 0; i < count; ++i)
        emit("latency", workload, i, samples[i]);
    emit("batch", workload, count, batch);
}

int main(int argc, char **argv)
{
    if (argc != 5 || strspn(argv[1], "0123456789") != strlen(argv[1]) ||
        strlen(argv[1]) == 0 || strlen(argv[1]) > 5 ||
        strtoul(argv[1], NULL, 10) == 0 || strtoul(argv[1], NULL, 10) > 65535 ||
        strlen(argv[3]) != 32 || strspn(argv[3], "0123456789abcdef") != 32 ||
        (strcmp(argv[4], "main") != 0 && strcmp(argv[4], "profile") != 0)) {
        fprintf(stderr, "usage: query-client PORT CSV CHECKSUM main|profile\n");
        return EXIT_FAILURE;
    }
    checksum = argv[3];
    output = fopen(argv[2], "wx");
    if (!output)
        fail("cannot create fresh CSV");
    if (fprintf(output, "phase,workload,index,elapsed_ns\n") < 0)
        fail("CSV header write failed");
    const char *keys[] = {"host", "port", "user", "dbname", "connect_timeout", "sslmode", NULL};
    const char *values[] = {"127.0.0.1", argv[1], "phase2", "postgres", "2", "disable", NULL};
    uint64_t start = now_ns();
    connection = PQconnectdbParams(keys, values, 0);
    uint64_t elapsed = now_ns() - start;
    if (!connection || PQstatus(connection) != CONNECTION_OK)
        fail("connection failed");
    emit("connection", "connect", 0, elapsed);
    start = now_ns();
    PGresult *first = query("SELECT 1;");
    elapsed = now_ns() - start;
    const char *one[] = {"1"};
    expect(first, 1, one);
    emit("first_query", "select", 0, elapsed);
    const char *settings[] = {"on", "on", "fsync", "64kB", "0", "0", "2", "C", "C", "16.15"};
    expect(query("SELECT current_setting('fsync'),current_setting('synchronous_commit'),"
                 "current_setting('wal_sync_method'),current_setting('work_mem'),"
                 "current_setting('max_parallel_workers'),current_setting('max_worker_processes'),"
                 "current_setting('max_connections'),datcollate,datctype,"
                 "current_setting('server_version') FROM pg_database "
                 "WHERE datname=current_database();"), 10, settings);
    PQclear(query("SET statement_timeout='10s';"));
    int profiled = strcmp(argv[4], "profile") == 0;
    unsigned warmup = profiled ? 10 : 50;
    run("select", warmup, profiled ? 50 : 1000);
    run("lookup", warmup, profiled ? 50 : 1000);
    run("commit", warmup, profiled ? 50 : 300);
    PQclear(query("SET statement_timeout='60s';"));
    run("spill", 1, profiled ? 1 : 5);
    char expected[32];
    snprintf(expected, sizeof(expected), "%u", counter);
    const char *final[] = {expected};
    expect(query("SELECT counter FROM eval_commits WHERE id=1;"), 1, final);
    PQfinish(connection);
    connection = NULL;
    if (fclose(output) != 0)
        fail("CSV close failed");
    puts("PASS query-client: exact results and bounded timing samples");
    return EXIT_SUCCESS;
}
