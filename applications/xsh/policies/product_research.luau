-- XSH Product policy.  This file is inert source: Rust admits its digest and
-- binds the named `factory` capability to the connection-owned operations.

local function handler(method)
    return [[
return function(call)
    local result = coroutine.yield({
        kind = "capability",
        capability = "factory",
        method = "]] .. method .. [[",
        arguments_json = call.arguments_json,
    })
    return {
        content = result.content,
        details_json = result.details_json,
        is_error = result.is_error,
        terminate = result.terminate,
    }
end
]]
end

local function tool(name, description, schema_json, method)
    return {
        name = name,
        description = description,
        capability = "factory",
        execution_mode = "sequential",
        schema_json = schema_json,
        handler_source = handler(method),
    }
end

local read_schema = [[
{"type":"object","additionalProperties":false,"required":["repository_relative_path"],"properties":{"repository_relative_path":{"type":"string","minLength":1}}}
]]
local write_schema = [[
{"type":"object","additionalProperties":false,"required":["repository_relative_path","contents"],"properties":{"repository_relative_path":{"type":"string","minLength":1},"contents":{"type":"string"}}}
]]
local edit_schema = [[
{"type":"object","additionalProperties":false,"required":["repository_relative_path","old_text","new_text"],"properties":{"repository_relative_path":{"type":"string","minLength":1},"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}}}
]]
local search_schema = [[
{"type":"object","additionalProperties":false,"required":["query"],"properties":{"query":{"type":"string","minLength":1,"maxLength":4096},"repository_relative_path":{"type":"string","minLength":1},"limit":{"type":"integer","minimum":1,"maximum":200}}}
]]
local list_schema = [[
{"type":"object","additionalProperties":false,"properties":{"repository_relative_path":{"type":"string","minLength":1}}}
]]
local shell_schema = [[
{"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string","minLength":1,"maxLength":65536},"timeout_millis":{"type":"integer","minimum":1,"maximum":300000}}}
]]
local artifact_seal_schema = [[
{"type":"object","additionalProperties":false,"required":["workspace_relative_path","byte_limit"],"properties":{"workspace_relative_path":{"type":"string","minLength":1},"byte_limit":{"type":"integer","minimum":1}}}
]]
local terminal_schema = [[
{"type":"object","additionalProperties":false}
]]
local product_submit_schema = [[
{"type":"object","additionalProperties":false,"required":["title","mission_value","scope","contract_owner","risk","narrative","evidence","acceptance_criteria","contract_reads","duplicate_search","reproducer_profile","reproducer"],"properties":{"title":{"type":"string","minLength":1,"maxLength":240},"mission_value":{"type":"string","minLength":1,"maxLength":4096},"scope":{"type":"string","minLength":1,"maxLength":4096},"contract_owner":{"type":"string","minLength":1,"maxLength":240},"risk":{"type":"string","minLength":1,"maxLength":4096},"narrative":{"type":"object","additionalProperties":false,"required":["artifact_id","digest","byte_length"],"properties":{"artifact_id":{"type":"integer","minimum":1},"digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"byte_length":{"type":"integer","minimum":0}}},"evidence":{"type":"object","additionalProperties":false,"required":["artifact_id","digest","byte_length"],"properties":{"artifact_id":{"type":"integer","minimum":1},"digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},"byte_length":{"type":"integer","minimum":0}}},"acceptance_criteria":{"type":"array","minItems":1,"items":{"type":"string","minLength":1,"maxLength":4096}},"contract_reads":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["path","reason"],"properties":{"path":{"type":"string","minLength":1},"reason":{"type":"string","minLength":1,"maxLength":4096}}}},"duplicate_search":{"type":"object","additionalProperties":false,"required":["query","limit"],"properties":{"query":{"type":"string","minLength":1,"maxLength":4096},"limit":{"type":"integer","minimum":1,"maximum":20}}},"reproducer_profile":{"type":"string","minLength":1,"maxLength":160},"reproducer":{"type":"object","additionalProperties":false,"required":["comparison_rule_version","command","stdin","expected_observation","first_observation","second_observation"],"properties":{"comparison_rule_version":{"type":"integer","const":1},"command":{"type":"object"},"stdin":{"type":["object","null"]},"expected_observation":{"type":"object"},"first_observation":{"type":"object"},"second_observation":{"type":"object"}}}}}
]]

local allowed = {
    workspace_read = true,
    workspace_search = true,
    workspace_list = true,
    shell = true,
    artifact_seal = true,
    product_submit_ticket = true,
    work_complete = true,
}

return {
    system_prompt_append = "",
    tools = {
        tool("workspace_read", "Read exact bytes from one path in the assigned workspace.", read_schema, "workspace.read"),
        tool("workspace_search", "Search the assigned workspace with bounded filters.", search_schema, "workspace.search"),
        tool("workspace_list", "List entries beneath the assigned workspace.", list_schema, "workspace.list"),
        tool("shell", "Run one bounded command in the assigned workspace.", shell_schema, "shell.exec"),
        tool("artifact_seal", "Seal one workspace file as immutable assignment evidence.", artifact_seal_schema, "artifact.seal_workspace_file"),
        tool("product_submit_ticket", "Submit one complete reproducible XSH defect proposal.", product_submit_schema, "product.submit_ticket"),
        tool("work_complete", "Complete this assignment without another proposal.", terminal_schema, "work.complete"),
    },
    before_tool_call = function(call)
        if allowed[call.name] then
            return "allow"
        end
        return { action = "block", reason = "that tool is not admitted for this assignment" }
    end,
}
