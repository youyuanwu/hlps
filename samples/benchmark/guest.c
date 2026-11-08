#include <hyperlight_guest.h>

static uint64_t step(uint64_t value)
{
    value ^= value >> 12;
    value ^= value << 25;
    value ^= value >> 27;
    return value * UINT64_C(2685821657736338717);
}

static hl_ReturnValue *add(const hl_FunctionCall *call)
{
    int64_t left = call->parameters[0].value.Int;
    int64_t right = call->parameters[1].value.Int;
    return hl_result_from_Long(left + right);
}

static hl_ReturnValue *echo_bytes(const hl_FunctionCall *call)
{
    hl_Vec bytes = call->parameters[0].value.VecBytes;
    return hl_result_from_Bytes(bytes.data, bytes.len);
}

static hl_ReturnValue *call_host(const hl_FunctionCall *call)
{
    uint64_t value = call->parameters[0].value.ULong;
    uint32_t count = call->parameters[1].value.UInt;
    hl_Parameter parameter = {.tag = hl_ParameterType_ULong};
    hl_FunctionCall host_call = {
        .function_name = "HostStep",
        .parameters = &parameter,
        .parameters_len = 1,
        .return_type = hl_ReturnType_ULong,
    };
    for (uint32_t i = 0; i < count; ++i) {
        parameter.value.ULong = value;
        hl_call_host_function(&host_call);
        value = hl_get_host_return_value_as_ULong();
    }
    return hl_result_from_ULong(value);
}

static hl_ReturnValue *compute(const hl_FunctionCall *call)
{
    uint64_t value = call->parameters[0].value.ULong;
    uint32_t count = call->parameters[1].value.UInt;
    for (uint32_t i = 0; i < count; ++i) {
        value = step(value);
    }
    return hl_result_from_ULong(value);
}

void hyperlight_main(void)
{
    const hl_ParameterType add_parameters[] = {
        hl_ParameterType_Int, hl_ParameterType_Int,
    };
    const hl_ParameterType echo_parameters[] = {hl_ParameterType_VecBytes};
    const hl_ParameterType loop_parameters[] = {
        hl_ParameterType_ULong, hl_ParameterType_UInt,
    };
    hl_register_function_definition("Add", add, 2, add_parameters,
                                    hl_ReturnType_Long);
    hl_register_function_definition("EchoBytes", echo_bytes, 1, echo_parameters,
                                    hl_ReturnType_VecBytes);
    hl_register_function_definition("CallHost", call_host, 2, loop_parameters,
                                    hl_ReturnType_ULong);
    hl_register_function_definition("Compute", compute, 2, loop_parameters,
                                    hl_ReturnType_ULong);
}
