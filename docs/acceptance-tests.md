# AgentHub 구현 및 인수 보고서

2026-11-19, Design v1.0 및 Q1~Q46 기준. Windows x64 호스트와 두 Linux Helper를 빌드했습니다. 현재 판정은 **8개 Pass, Windows 실기가 남은 2개 Not Run**입니다.

## 구현 범위

Cargo workspace의 `core`에 모델·탐색·정책·변경·복원·진단·런타임을 구성했습니다. `tui`에는 마법사·탐색기·비교·편집 폼·Diff 승인·작업 처리를, `helper`에는 비상주 Linux 진입점을 구현했습니다. 세부 빌드 방법, CLI 버전, 지원 형식과 제한은 README 및 호환성 표에 있습니다.

## 시나리오별 결과

| ID | 판정 | 실행 근거 |
|---|---|---|
| AT-01 | Pass | 세 CLI가 설치된 격리 환경에서 fixture의 15개 등록과 7종 자산 발견. 공유 Skill의 두 논리 ID와 같은 물리 파일 ID 확인. |
| AT-02 | Pass | 전역·프로젝트 동명 MCP 및 비활성 등록을 검증. 실제 Codex 조회가 선택한 등록에만 Verified 표시. |
| AT-03 | Pass | 이름·설명·본문 검색과 필터를 검증. 실제 PTY에서 외부 편집 후 30초 보완 스캔 이전에 검색 결과가 갱신됨. |
| AT-04 | Pass | 세 네이티브 설정 형식의 CRUD·재파싱, JSONC 주석 및 기존 필드 보존. 실제 설치 CLI와 TUI 폼→계획→승인→적용 경로를 격리된 테스트 키로 검증. |
| AT-05 | Pass | 전체 폴더의 복제·이름 변환, 상대 참조 보존, 미지원 필드 및 외부 참조 경고, 심볼릭 링크 차단, 테스트 실행 마커 부재 확인. |
| AT-06 | Pass | 명시적 승인 전 서버 미기동, stdio 초기화·목록·실패·시간 초과·취소·과다 출력 검사. 루프백 Streamable HTTP 세션 헤더와 다음 페이지 요청 검사. 요청 기록에는 도구 호출이 없음. |
| AT-07 | Not Run | Linux에서 승인 거부, 백업 잠금, 암호문 변조, 충돌, 전체·선택 복원, 롤백 성공·부분 실패 통과. 이때 사용한 암호화는 테스트 전용 XChaCha20-Poly1305. 실제 Windows 사용자 DPAPI와 Windows 파일 교체 확인이 남음. |
| AT-08 | Pass | 잘못된 CLI 설정·관리형 정책·버전 불명·호환되지 않는 Helper 응답을 주입. 한 환경의 실패 후 호스트 목록 및 상세 보기 유지 확인. |
| AT-09 | Pass | 합성 인증값을 넣고 Registry 직렬화, 마스킹된 Diff, 실제 터미널 화면, 감사 로그, 내보낸 JSON과 암호화 백업에서 평문 비노출 확인. |
| AT-10 | Not Run | Linux CLI 세 개의 실제 명령, x86_64 Helper의 JSON 왕복·종료, PTY 마법사 통과. Windows에서 실제 `wsl.exe`를 통한 설치·왕복·마법사 검증이 남음. |

## 재현과 증거

Debian GNU/Linux 13, x86_64, Rust 1.100.0. 설치 CLI는 Codex 0.167.0-alpha.4, OpenCode 1.19.6, Claude Code 2.1.327입니다. Linux 자동화는 임시 프로젝트·CLI 전용 설정을 사용했습니다. Windows 산출물은 LLVM-MinGW로 교차 빌드했고, 두 Helper는 musl 정적 실행 파일입니다. ARM64 Helper는 빌드 및 ELF 헤더를 확인했습니다.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p agenthub live_tui -- --ignored
cargo build --locked --workspace
python3 tests/live_cli_smoke.py target/debug/agenthub target/debug/agenthub-wsl-helper
python3 tests/tui_smoke.py target/debug/agenthub
```

`live_tui`와 두 Python 실행은 위 버전의 CLI가 PATH에 있어야 합니다. PTY 검사는 Linux와 Python 패키지 `pyte==0.8.2`를 사용합니다. 루프백 HTTP 테스트는 로컬 소켓 접근이 필요합니다. [실행 기록](evidence/linux-verification.txt)

Windows PC에서는 패키지의 `scripts/Test-Windows.ps1`를 실행합니다. 스크립트가 기록하는 `acceptance.json`에 OS·배포판·CLI 버전·DPAPI 결과·마법사 결과가 들어갑니다. 실기가 통과하기 전까지 MVP 인수는 미완료입니다.
