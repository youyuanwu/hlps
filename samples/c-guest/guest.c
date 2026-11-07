#include <hyperlight_guest.h>
#include <stdio.h>

static hl_ReturnValue *greet(const hl_FunctionCall *call)
{
    char message[256];
    int length = snprintf(message, sizeof(message),
                          "Hello, %s! This is a Hyperlight C guest.",
                          call->parameters[0].value.String);
    if (length < 0 || (size_t)length >= sizeof(message)) {
        hl_set_error(hl_ErrorCode_GuestError, "Greeting exceeds the 255-byte limit");
        return NULL;
    }

    hl_Parameter parameter = {
        .tag = hl_ParameterType_String,
        .value.String = message,
    };
    hl_FunctionCall host_call = {
        .function_name = "HostLog",
        .parameters = &parameter,
        .parameters_len = 1,
        .return_type = hl_ReturnType_Bool,
    };
    hl_call_host_function(&host_call);
    if (!hl_get_host_return_value_as_Bool()) {
        hl_set_error(hl_ErrorCode_GuestError, "HostLog did not acknowledge the message");
        return NULL;
    }

    return hl_result_from_String(message);
}

static hl_ReturnValue *add(const hl_FunctionCall *call)
{
    int64_t left = call->parameters[0].value.Int;
    int64_t right = call->parameters[1].value.Int;
    return hl_result_from_Long(left + right);
}

void hyperlight_main(void)
{
    const hl_ParameterType greet_parameters[] = {hl_ParameterType_String};
    const hl_ParameterType add_parameters[] = {
        hl_ParameterType_Int, hl_ParameterType_Int,
    };
    hl_register_function_definition("Greet", greet, 1, greet_parameters,
                                    hl_ReturnType_String);
    hl_register_function_definition("Add", add, 2, add_parameters,
                                    hl_ReturnType_Long);
}
