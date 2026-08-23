// context-probe: print a Claude Code session's current context fill in tokens.
//
// Usage: context-probe <session.jsonl>
//
// Core borrowed from tam-tools/style-checks/src/status-line.cc
// (getCurrentContextSize): tail the last 50 lines of the JSONL, scan
// backwards, skip sidechain entries, take the latest assistant/tool_result
// entry with a usage object, and report
// input_tokens + cache_read_input_tokens + cache_creation_input_tokens.
//
// Prints the integer on stdout. Prints 0 (exit 0) when no usage entry is
// found yet; exits 1 only on usage error.

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

int main(int argc, char** argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: context-probe <session.jsonl>\n");
        return 1;
    }

    char cmd[4096 + 32];
    int ret = snprintf(cmd, sizeof(cmd), "tail -n 50 \"%s\" 2>/dev/null", argv[1]);
    if (ret >= (int)sizeof(cmd) || ret < 0) {
        fprintf(stderr, "context-probe: path too long\n");
        return 1;
    }

    FILE* pipe = popen(cmd, "r");
    if (!pipe) {
        fprintf(stderr, "context-probe: tail failed: %s\n", strerror(errno));
        return 1;
    }

    std::vector<std::string> lines;
    char buffer[4096];
    while (fgets(buffer, sizeof(buffer), pipe) != nullptr) {
        lines.emplace_back(buffer);
    }
    pclose(pipe);

    int context_size = 0;
    for (auto it = lines.rbegin(); it != lines.rend(); ++it) {
        const std::string& line = *it;

        if (line.find("\"isSidechain\":true") != std::string::npos) continue;

        bool is_assistant = line.find("\"type\":\"assistant\"") != std::string::npos;
        bool is_tool_result = line.find("\"type\":\"tool_result\"") != std::string::npos;
        if (!is_assistant && !is_tool_result) continue;

        const char* usage_start = strstr(line.c_str(), "\"usage\":{");
        if (!usage_start) continue;

        int input_tokens = 0, cache_read = 0, cache_creation = 0;
        const char* p;
        if ((p = strstr(usage_start, "\"input_tokens\":")))
            input_tokens = strtol(p + 15, nullptr, 10);
        if ((p = strstr(usage_start, "\"cache_read_input_tokens\":")))
            cache_read = strtol(p + 26, nullptr, 10);
        if ((p = strstr(usage_start, "\"cache_creation_input_tokens\":")))
            cache_creation = strtol(p + 30, nullptr, 10);

        context_size = input_tokens + cache_read + cache_creation;
        break;
    }

    printf("%d\n", context_size);
    return 0;
}
