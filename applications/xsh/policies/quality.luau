-- XSH Quality policy.  It is descriptive policy; Rust owns every effect.

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
local forum_search_schema = [[
{"type":"object","additionalProperties":false,"required":["query","limit"],"properties":{"query":{"type":"string","minLength":1,"maxLength":4096},"topic_id":{"type":["integer","null"],"minimum":1},"thread_id":{"type":["integer","null"],"minimum":1},"post_kind":{"enum":["Note","Question","Finding","Proposal","Challenge","Correction","DecisionLink",null]},"created_after_micros":{"type":["integer","null"],"minimum":0},"created_before_micros":{"type":["integer","null"],"minimum":0},"cursor":{"type":["string","null"],"maxLength":512},"limit":{"type":"integer","minimum":1,"maximum":20}}}
]]
local forum_topics_schema = [[
{"type":"object","additionalProperties":false,"required":["limit"],"properties":{"cursor":{"type":["string","null"],"maxLength":512},"limit":{"type":"integer","minimum":1,"maximum":20}}}
]]
local forum_threads_schema = [[
{"type":"object","additionalProperties":false,"required":["topic_id","limit"],"properties":{"topic_id":{"type":"integer","minimum":1},"cursor":{"type":["string","null"],"maxLength":512},"limit":{"type":"integer","minimum":1,"maximum":20}}}
]]
local forum_read_schema = [[
{"type":"object","additionalProperties":false,"required":["thread_id","limit"],"properties":{"thread_id":{"type":"integer","minimum":1},"after_post_id":{"type":["integer","null"],"minimum":1},"limit":{"type":"integer","minimum":1,"maximum":20}}}
]]
local publication_schema = [[
{"type":"object","additionalProperties":false,"required":["anchor","kind","summary","body_artifact_id"],"properties":{"anchor":{"type":"object","additionalProperties":false,"required":["kind","id"],"properties":{"kind":{"enum":["project","rfc","rfc_revision","ticket","ticket_revision","experiment","claim","decision"]},"id":{"type":"integer","minimum":1}}},"kind":{"enum":["finding","question","challenge","correction","decision_link","note"]},"summary":{"type":"string","minLength":1,"maxLength":4096},"body_artifact_id":{"type":"integer","minimum":1},"attachments":{"type":"array","maxItems":8,"items":{"type":"object","additionalProperties":false,"required":["artifact_id","label"],"properties":{"artifact_id":{"type":"integer","minimum":1},"label":{"type":"string","minLength":1,"maxLength":160}}}},"reply_to":{"type":["integer","null"],"minimum":1},"supersedes":{"type":["integer","null"],"minimum":1}}}
]]
local artifact_seal_schema = [[
{"type":"object","additionalProperties":false,"required":["workspace_relative_path","byte_limit"],"properties":{"workspace_relative_path":{"type":"string","minLength":1},"byte_limit":{"type":"integer","minimum":1}}}
]]
local artifact_read_schema = [[
{"type":"object","additionalProperties":false,"required":["artifact_id","expected_digest"],"properties":{"artifact_id":{"type":"integer","minimum":1},"expected_digest":{"type":"string","pattern":"^[a-f0-9]{64}$"}}}
]]
local full_suite_schema = [[
{"type":"object","additionalProperties":false,"required":["validation_profile"],"properties":{"validation_profile":{"type":"string","minLength":1,"maxLength":160}}}
]]
local review_schema = [[
{"type":"object","additionalProperties":false,"required":["full_suite_validation_id","verdict","rationale","risks","additional_probes"],"properties":{"full_suite_validation_id":{"type":"integer","minimum":1},"verdict":{"type":"string","enum":["accept","reject"]},"rationale":{"type":"object"},"risks":{"type":"object"},"additional_probes":{"type":"object"}}}
]]
local allowed = {
    workspace_read = true,
    workspace_write = true,
    workspace_edit = true,
    workspace_search = true,
    workspace_list = true,
    shell = true,
    forum_search = true,
    forum_list_topics = true,
    forum_list_threads = true,
    forum_read_thread = true,
    publication_create = true,
    artifact_seal = true,
    artifact_read = true,
    quality_run_full_suite = true,
    quality_submit_review = true,
}

return {
    system_prompt_append = "",
    tools = {
        tool("workspace_read", "Read exact bytes from one path in the assigned workspace.", read_schema, "workspace.read"),
        tool("workspace_write", "Write beneath the assigned workspace.", write_schema, "workspace.write"),
        tool("workspace_edit", "Apply a bounded edit beneath the assigned workspace.", edit_schema, "workspace.edit"),
        tool("workspace_search", "Search the assigned workspace with bounded filters.", search_schema, "workspace.search"),
        tool("workspace_list", "List entries beneath the assigned workspace.", list_schema, "workspace.list"),
        tool("shell", "Run one bounded command in the assigned workspace.", shell_schema, "shell.exec"),
        tool("forum_search", "Search discussion history with bounded filters and continuation.", forum_search_schema, "forum.search"),
        tool("forum_list_topics", "List discussion topics by recent activity.", forum_topics_schema, "forum.list_topics"),
        tool("forum_list_threads", "List threads in one discussion topic by recent activity.", forum_threads_schema, "forum.list_threads"),
        tool("forum_read_thread", "Read a bounded chronological page of discussion posts.", forum_read_schema, "forum.read_thread"),
        tool("publication_create", "Publish one immutable anchored finding, question, challenge, correction, decision link, or note.", publication_schema, "publication.create"),
        tool("artifact_seal", "Seal one workspace file as immutable assignment evidence.", artifact_seal_schema, "artifact.seal_workspace_file"),
        tool("artifact_read", "Read one sealed upstream evidence item named in this assignment.", artifact_read_schema, "artifact.read"),
        tool("quality_run_full_suite", "Run the assigned full validation suite on the exact candidate.", full_suite_schema, "quality.run_full_suite"),
        tool("quality_submit_review", "Submit the independent review of the exact candidate.", review_schema, "quality.submit_review"),
    },
    before_tool_call = function(call)
        if allowed[call.name] then
            return "allow"
        end
        return { action = "block", reason = "that tool is not admitted for this assignment" }
    end,
}
