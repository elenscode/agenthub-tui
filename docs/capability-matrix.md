# CLI 호환성 기준

2026-11-19 Linux x86_64의 격리된 경로에서 아래 버전의 네이티브 CLI를 실행했습니다. 쓰기 허용 여부는 실행 시 각 환경에서 다시 판정합니다. 미확인 버전은 위치와 원본을 탐색하고 `Unknown` 및 읽기 전용으로 표시합니다.

| CLI | 검증한 버전 | 설정 형식 및 검증 |
|---|---|---|
| Codex | 0.167.0-alpha.4 | TOML의 `mcp_servers`. `mcp get --json`의 transport와 로컬 등록이 유일하게 일치할 때 해당 이름의 인식·활성화 상태를 확인합니다. 프로젝트 신뢰 설정에 따라 전역 등록이 선택되는 사례를 재현했습니다. |
| OpenCode | 1.19.6 | JSON·JSONC의 `mcp`. local command 배열과 remote URL 형식을 구분합니다. 격리된 프로젝트에서 `opencode debug config --pure`가 비활성 MCP 항목을 읽는 것을 확인했습니다. |
| Claude Code | 2.1.327 | 프로젝트 `.mcp.json`의 `mcpServers`, 사용자 설정의 전역·프로젝트별 등록을 읽습니다. 실제 `claude mcp add-json --scope project`로 임시 프로젝트 형식을 확인했습니다. 사용자 승인·정책에 따른 활성 여부는 확인 가능한 범위까지만 표시합니다. |

Codex의 프로젝트·사용자 설정과 agents, instructions, Skill 경로, 로컬 플러그인 캐시를 탐색합니다. OpenCode는 프로젝트·사용자 설정, agent·command·plugin 디렉터리, 공유 Skill, 설정의 로컬 참조를 탐색합니다. Claude는 프로젝트·사용자·관리형 설정, Skill, agent, command, rules, hooks, 설치된 플러그인의 로컬 경로를 탐색합니다. 원격 카탈로그는 참조로 표시합니다. OpenCode 플러그인이 실행 중 등록하는 Hook, 미해결 참조, 설치 버전을 확인할 수 없는 기능의 적용 상태는 Unknown입니다.

원본 위치에서 직접 증명할 수 없는 동명 우선순위는 상세 근거에 남깁니다. 발견·설정·실제 인식과 MCP 연결 성공은 독립적인 상태입니다. 공식 스키마와 설치 상태가 맞지 않거나 보존형 파싱이 실패하면 쓰기 계획을 중단합니다.

MCP 수동 진단은 stdio와 Streamable HTTP를 지원합니다. SSE 형식의 Streamable HTTP 응답을 읽을 수 있습니다. 독립적인 레거시 SSE·WebSocket 전송이나 CLI 전용 인증 도우미가 필요한 연결은 CLI에서 확인합니다. Codex는 `codex mcp login`, OpenCode는 `opencode mcp auth`, Claude Code는 세션의 `/mcp`에서 해당 서버의 인증 흐름을 시작할 수 있습니다.

[Codex 버전별 공식 스키마](https://raw.githubusercontent.com/openai/codex/rust-v0.167.0-alpha.4/codex-rs/core/config.schema.json) · [Codex 조회 명령 소스](https://github.com/openai/codex/blob/rust-v0.167.0-alpha.4/codex-rs/cli/src/mcp_cmd.rs) · [OpenCode 공식 스키마](https://opencode.ai/config.json) · [Claude Code 설정](https://code.claude.com/docs/en/settings) · [Claude Code MCP](https://code.claude.com/docs/en/mcp)
