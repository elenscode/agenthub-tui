# AgentHub TUI

Linux 터미널에서 폴더를 열고 Codex, OpenCode, Claude Code의 프로젝트 설정과 사용자 전역 설정을 함께 살펴볼 수 있습니다. MCP, Skill, Agent, Command, Instructions, Plugin, Hook을 탐색하며 MCP와 Skill은 편집할 수 있습니다.

## 실행

Linux x86_64 배포판에서는 패키지를 풀고 실행합니다.

```sh
tar -xzf AgentHub-0.2.0-Linux-x86_64.tar.gz
./AgentHub-0.2.0-Linux-x86_64/agenthub ~/work/my-project
```

`agenthub`를 PATH에 두면 어느 폴더에서나 실행할 수 있습니다. `agenthub`는 현재 디렉터리에서 시작하고 `agenthub <폴더>`는 지정한 디렉터리를 엽니다. `--help`와 `--version`으로 사용법과 버전을 확인합니다.

## 폴더 이동

`f`를 눌러 왼쪽 폴더 트리로 이동합니다. ↑/↓로 폴더를 고르고 →로 펼치거나 첫 자식으로 들어갑니다. ←로 접거나 부모로 이동하고, Backspace로 트리의 시작 위치보다 위로도 올라갈 수 있습니다. **Enter**를 누르면 선택한 폴더가 현재 프로젝트가 되고 자산 목록과 파일 감시가 갱신됩니다. 폴더만 둘러봤다면 `f` 또는 Esc로 자산 목록에 돌아갑니다.

트리는 숨김 디렉터리를 포함하고 심볼릭 링크는 건너뜁니다. 폴더에 포커스가 있을 때 `r`로 구조를 다시 읽습니다. 좁은 터미널에서는 `f`로 폴더와 자산 화면을 전환합니다.

## 자산 탐색과 변경

| 자산 목록에서 누르는 키 | 동작 |
|---|---|
| ↑/↓ 또는 j/k | 자산 선택 |
| h/l | 종류별 접기·펴기 |
| Enter, Tab, Esc | 상세 열기, 개요·원본·Diff·상태 전환, 돌아가기 |
| / | 이름·설명·본문 검색 |
| 1, 2, 3, 4 | CLI, 종류, 범위, 상태 필터 전환 |
| Space | 비교 기준 고정. 다른 자산의 Diff 탭에서 차이 확인 |
| : | 명령 팔레트 열기. Tab으로 명령 완성 |
| r, !, x, ?, q | 다시 탐색, 상태 보기, 대기 작업 취소, 도움말, 종료 |

- `:new-mcp`, `:new-skill`, `:edit`, `:clone`, `:delete`로 변경안을 만듭니다. Skill 본문은 `VISUAL`, `EDITOR`, `vi` 순서로 선택한 편집기에서 수정합니다.
- `:import-mcp`는 로컬 설정 파일을, `:import-skill`은 Skill 폴더를 가져옵니다. `:convert`는 Skill을 선택한 CLI·범위·이름으로 복사합니다.
- 변경 미리보기에서 영향받는 등록과 차이를 확인하고 `a`로 승인합니다. 자격증명이 복사되는 경우 `y`로 한 번 더 확인합니다. 승인 전에 Esc를 누르면 취소됩니다.
- `:history`에서 백업 ID를 찾고, 해당 프로젝트에서 `:restore`를 열어 전체 변경이나 Skill 내부 상대 경로를 복원합니다.
- `:diagnose`는 확인 후 선택한 MCP 서버의 초기화와 도구 목록 조회를 실행합니다. `:export`는 승인 후 로컬에 진단 요약 JSON을 저장합니다.

백업과 감사 기록의 기본 위치는 `${XDG_STATE_HOME:-~/.local/state}/agenthub`입니다. `AGENTHUB_STATE_DIR`에 절대 경로를 지정해 바꿀 수 있습니다. 백업 복구에는 `backups/`의 스냅샷과 `keys/backup.key`가 함께 필요합니다. 검증된 CLI 버전과 지원 형식은 [호환성 표](docs/capability-matrix.md)에서 확인할 수 있습니다.

## 소스에서 빌드

Rust stable과 Linux용 링커가 필요합니다.

```sh
cargo build --locked --release -p agenthub
./target/release/agenthub .
```

[설계 기준](docs/design-baseline.md) · [검증 방법](docs/acceptance-tests.md) · [보호 및 복구](docs/threat-model.md)
