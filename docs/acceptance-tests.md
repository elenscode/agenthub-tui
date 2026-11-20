# Linux 검증

## 자동 테스트

Rust stable, Python 3, 로컬 루프백 소켓이 필요합니다. 기본 테스트는 임시 프로젝트와 설정 파일을 사용합니다.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

핵심 테스트는 3개 CLI·7종 자산의 탐색, 등록 우선순위, 본문 검색, 설정 편집과 형식 보존, Skill 복사와 변환, MCP 진단, 암호화 백업, 전체·선택 복원, 충돌 처리와 비밀정보 마스킹을 확인합니다. 폴더 테스트는 펼치기·접기, 부모 이동, 삭제된 선택의 복구, 링크 제외와 프로젝트 전환 후 이전 탐색 결과 폐기를 검사합니다.

## 실제 CLI와 터미널

아래 검사는 [호환성 표](capability-matrix.md)의 세 CLI가 PATH에 설치된 Linux 환경에서 실행합니다. PTY 화면 검사는 Python 환경에 `pyte==0.8.2`가 필요합니다.

```sh
cargo test --locked -p agenthub live_tui -- --ignored
cargo build --locked --release -p agenthub
python3 tests/live_cli_smoke.py
python3 tests/tui_smoke.py target/release/agenthub
```

PTY 검사는 실제 키 입력으로 폴더 이동, 좁은 터미널 화면, 프로젝트 전환과 파일 감시, 변경 승인·취소, 백업 복원, 진단 요약 저장과 종료를 확인합니다.
