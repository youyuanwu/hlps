#include <stdio.h>
#include <string.h>
#include <hyperlight_guest.h>

#define CHECK(condition) \
    do { \
        if (!(condition)) { \
            printf("FAIL line %d: %s\n", __LINE__, #condition); \
            return 1; \
        } \
    } while (0)

static hl_CGuestFunc greet_callback;
static hl_CGuestFunc add_callback;
static int registration_errors;
static hl_ErrorCode error_code;
static char error_message[256];
static char logged_message[256];
static char returned_message[256];
static hl_ReturnValue result;
static bool host_acknowledges = true;
static bool host_call_valid;
static int host_calls;

void hl_register_function_definition(const char *name, hl_CGuestFunc callback,
                                     uintptr_t count, const hl_ParameterType *types,
                                     hl_ReturnType return_type)
{
    if (strcmp(name, "Greet") == 0 && count == 1 &&
        types[0] == hl_ParameterType_String && return_type == hl_ReturnType_String) {
        greet_callback = callback;
    } else if (strcmp(name, "Add") == 0 && count == 2 &&
               types[0] == hl_ParameterType_Int && types[1] == hl_ParameterType_Int &&
               return_type == hl_ReturnType_Long) {
        add_callback = callback;
    } else {
        registration_errors++;
    }
}

void hl_set_error(hl_ErrorCode code, const char *message)
{
    error_code = code;
    snprintf(error_message, sizeof(error_message), "%s", message);
}

void hl_call_host_function(const hl_FunctionCall *call)
{
    host_calls++;
    host_call_valid = strcmp(call->function_name, "HostLog") == 0 &&
                      call->parameters_len == 1 &&
                      call->parameters[0].tag == hl_ParameterType_String &&
                      call->return_type == hl_ReturnType_Bool;
    if (host_call_valid) {
        snprintf(logged_message, sizeof(logged_message), "%s",
                 call->parameters[0].value.String);
    }
}

bool hl_get_host_return_value_as_Bool(void)
{
    return host_acknowledges;
}

hl_ReturnValue *hl_result_from_String(const char *value)
{
    snprintf(returned_message, sizeof(returned_message), "%s", value);
    result.tag = hl_ReturnType_String;
    result.value.String = returned_message;
    return &result;
}

hl_ReturnValue *hl_result_from_Long(int64_t value)
{
    result.tag = hl_ReturnType_Long;
    result.value.Long = value;
    return &result;
}

int main(void)
{
    hyperlight_main();
    CHECK(registration_errors == 0 && greet_callback && add_callback);

    hl_Parameter name = {
        .tag = hl_ParameterType_String,
        .value.String = "Hyperlight",
    };
    hl_FunctionCall greeting = {
        .function_name = "Greet",
        .parameters = &name,
        .parameters_len = 1,
        .return_type = hl_ReturnType_String,
    };
    hl_ReturnValue *value = greet_callback(&greeting);
    CHECK(value && value->tag == hl_ReturnType_String);
    CHECK(strcmp(value->value.String,
                 "Hello, Hyperlight! This is a Hyperlight C guest.") == 0);
    CHECK(host_call_valid && host_calls == 1);
    CHECK(strcmp(logged_message, value->value.String) == 0);

    name.value.String = "";
    value = greet_callback(&greeting);
    CHECK(value && strcmp(value->value.String,
                          "Hello, ! This is a Hyperlight C guest.") == 0);

    char boundary_name[256];
    size_t max_name = 255 - strlen("Hello, ! This is a Hyperlight C guest.");
    memset(boundary_name, 'x', max_name);
    boundary_name[max_name] = '\0';
    name.value.String = boundary_name;
    value = greet_callback(&greeting);
    CHECK(value && strlen(value->value.String) == 255);

    int calls_before_error = host_calls;
    boundary_name[max_name] = 'x';
    boundary_name[max_name + 1] = '\0';
    CHECK(greet_callback(&greeting) == NULL);
    CHECK(error_code == hl_ErrorCode_GuestError);
    CHECK(strcmp(error_message, "Greeting exceeds the 255-byte limit") == 0);
    CHECK(host_calls == calls_before_error);

    error_code = hl_ErrorCode_NoError;
    host_acknowledges = false;
    name.value.String = "Hyperlight";
    CHECK(greet_callback(&greeting) == NULL);
    CHECK(error_code == hl_ErrorCode_GuestError);
    CHECK(strcmp(error_message, "HostLog did not acknowledge the message") == 0);

    hl_Parameter numbers[] = {
        {.tag = hl_ParameterType_Int, .value.Int = 20},
        {.tag = hl_ParameterType_Int, .value.Int = 22},
    };
    hl_FunctionCall addition = {
        .function_name = "Add",
        .parameters = numbers,
        .parameters_len = 2,
        .return_type = hl_ReturnType_Long,
    };
    value = add_callback(&addition);
    CHECK(value && value->tag == hl_ReturnType_Long && value->value.Long == 42);
    numbers[0].value.Int = INT32_MAX;
    numbers[1].value.Int = INT32_MAX;
    CHECK(add_callback(&addition)->value.Long == INT64_C(4294967294));
    numbers[0].value.Int = INT32_MIN;
    numbers[1].value.Int = INT32_MIN;
    CHECK(add_callback(&addition)->value.Long == -INT64_C(4294967296));
    numbers[1].value.Int = INT32_MAX;
    CHECK(add_callback(&addition)->value.Long == -1);

    puts("All native C guest checks passed.");
    return 0;
}
