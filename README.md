# AgentHub TUI

Windows Terminal에서 Codex, OpenCode, Claude Code의 현재 프로젝트와 사용자 전역 자산을 함께 살펴보는 도구입니다. MCP, Skill, Agent, Command, Instructions, Plugin, Hook을 탐색하고, MCP와 Skill을 편집할 수 있습니다.

## Windows에서 실행

Windows x64 패키지의 압축을 풀고 PowerShell에서 실행합니다.

```powershell
.\agenthub.exe
```

첫 실행에서 프로젝트를 선택합니다. WSL도 사용하려면 배포판과 Linux 프로젝트 경로를 선택합니다. 처음 설치하거나 Helper 버전이 바뀌면 대상 배포판 이름을 확인하고 `INSTALL`을 입력합니다. Windows 호스트에 x86_64·ARM64 Linux Helper가 포함되어 있습니다. WSL 연결에 실패하면 Windows 탐색을 계속할 수 있습니다.

```powershell
.\agenthub.exe --setup
.\agenthub.exe --project C:\work\example
.\agenthub.exe --scan --project C:\work\example
```

`--setup`으로 저장한 환경을 다시 선택할 수 있습니다. `--project`로 프로젝트를 일시 변경하면 해당 호스트 프로젝트를 엽니다.

## 탐색과 변경

| 키 | 동작 |
|---|---|
| ↑/↓ 또는 j/k, h/l | 자산 이동, 종류별 접기·펴기 |
| Enter, Tab, Esc | 상세 보기, 개요·원본·Diff·상태 전환, 뒤로 |
| / | 이름·설명·본문 검색 |
| 1~5 | CLI·종류·범위·환경·상태 필터 전환 |
| Space | 기준 자산 고정. 다른 자산의 Diff 탭에서 비교 |
| : | 명령 팔레트. Tab으로 명령 완성 |
| r, !, x, ? | 새로고침, 환경 상태, 실행 중인 작업 취소, 도움말 |

- `:new-mcp`, `:new-skill`, `:edit`, `:clone`, `:delete`로 변경안을 만듭니다. MCP 폼에는 실행 파일 또는 HTTP URL을 입력하고, 인자는 JSON 문자열 배열로 입력합니다. 기존 항목에서 비워 둔 필드는 유지합니다.
- `:import-mcp`는 대상 CLI 형식의 MCP 항목 파일 또는 같은 이름의 항목을 포함하는 설정 파일을 받습니다. `:import-skill`은 전체 Skill 폴더를 받습니다. 가져올 경로는 대상 환경의 절대 경로입니다.
- `:convert`에서 대상 환경·CLI·범위·이름을 선택합니다. 참조 파일과 스크립트도 함께 복사합니다. 미지원 필드와 외부 참조는 미리보기에 표시합니다.
- Skill 본문은 `VISUAL` 또는 `EDITOR`가 가리키는 실행 파일로 엽니다. Windows 기본값은 메모장입니다. 에디터에는 임시 Markdown 사본을 전달합니다.
- Diff에서 영향 받는 등록과 파일 변경을 확인하고 `a`로 승인합니다. 자격증명 복사 확인이 있으면 `y`를 추가로 입력합니다. 외부 변경 충돌이 발생하면 원본을 새로 읽어 Diff를 다시 검토합니다.
- `:history`에서 백업 ID를 찾고 `:restore`에서 선택합니다. Skill의 특정 파일·하위 폴더만 복원하려면 상대 경로를 입력합니다. 복원도 승인과 백업을 거칩니다.
- `:diagnose`는 선택한 MCP 서버를 실행하거나 해당 URL에 연결할 수 있습니다. `y`를 누르면 초기화와 도구 목록 조회를 수행합니다. 인증에는 각 CLI의 공식 흐름을 사용합니다.
- `:export`는 미리보기와 승인 후 로컬 JSON에 자산 수, 오류 수, CLI 버전, 환경 수를 저장합니다. `:retry`는 선택한 WSL에 재연결합니다.

로컬 상태는 기본적으로 Windows 사용자 LocalAppData의 `AgentHub` 디렉터리에 있습니다. `settings-v1.json`은 프로젝트와 배포판 선택을 저장합니다. 백업은 만든 Windows 사용자의 DPAPI로 보호합니다. 기본 보관 기준은 최근 30개, 최대 30일이며 변경 작업 뒤에 정리합니다.

## Windows·WSL 인수 검증

PowerShell에서 실제 사용할 배포판 이름을 지정합니다. `-InstallHelper`는 선택한 배포판에 번들 Helper를 설치하거나 갱신하도록 승인하는 옵션입니다. 이미 같은 Helper가 설치되어 있으면 이 옵션을 생략할 수 있습니다.

```powershell
.\scripts\Test-Windows.ps1 -Distro Ubuntu -InstallHelper
```

스크립트는 임시 프로젝트·설정·백업을 준비하고, 실제 DPAPI 복원, 세 CLI 버전, 7종 탐색, WSL 왕복과 첫 실행 마법사를 검사합니다. 마지막 마법사에서는 기본 임시 프로젝트와 지정한 배포판을 선택합니다. 검증 결과와 임시 디렉터리 위치는 PowerShell에 표시됩니다. 개발 환경에서 실행한 항목과 Windows 실기에서 남은 항목은 인수 보고서에서 확인할 수 있습니다. [인수 테스트 결과](docs/acceptance-tests.md)

## 소스에서 빌드

Rust stable, Cargo, 시스템 링커가 필요합니다. 이 버전은 Rust 1.100.0에서 빌드했습니다. Linux에서는 읽기 전용 TUI, 테스트, Helper를 실행할 수 있습니다.

```sh
cargo build --locked --workspace
cargo test --locked --workspace
cargo run --locked -p agenthub -- --project .
```

stdio 인수 테스트는 Python 3, HTTP 테스트는 로컬 루프백 소켓을 사용합니다. Windows 릴리스용 Helper는 Linux에서 x86_64 및 aarch64 `*-unknown-linux-musl` 타깃으로 각각 빌드합니다. Rust 타깃과 각 아키텍처의 C 도구체인을 준비한 뒤 생성된 Linux 실행 파일을 Windows 빌드에 전달합니다.

```powershell
.\scripts\package.ps1 -X64Helper .\helpers\x64\agenthub-wsl-helper -Arm64Helper .\helpers\arm64\agenthub-wsl-helper
```

공통 모델·어댑터·런타임·변경·진단은 `agenthub-core`의 개별 모듈에 있습니다. `agenthub`는 Ratatui 호스트이고 `agenthub-wsl-helper`는 단일 요청 후 종료하는 Linux 바이너리입니다. [호환성 표](docs/capability-matrix.md) · [보호 및 복구 경계](docs/threat-model.md) · [설계 기준선](docs/design-baseline.md)
