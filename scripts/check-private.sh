#!/usr/bin/env bash
# 개인정보나 컴퓨터 정보가 들어간 변경을 거절한다.
#   scripts/check-private.sh         커밋하려는(스테이징된) 변경 검사 — pre-commit 훅
#   scripts/check-private.sh --all   저장소 전체 검사 — CI
set -u

mode="${1:-staged}"
fail=0

report() {
  printf '거절: %s\n    %s\n' "$1" "$2"
  fail=1
}

if [ "$mode" = "--all" ]; then
  list() { git ls-files -z; }
  show() { cat -- "$1"; }
else
  list() { git diff --cached --name-only -z --diff-filter=ACMR; }
  show() { git show ":$1"; }
fi

# 올리면 안 되는 파일 종류: 녹화본, 프로젝트·설명 파일, 키, 환경 파일
banned_files='\.(mp4|mkv|mov|avi|wav|flac|pem|key|pfx|p12)$|\.tracks\.json$|\.trackcut\.json$|(^|/)\.env($|\.)|(^|/)id_(rsa|ed25519)'

names=(
  '사용자 폴더 경로(Windows)'
  '사용자 폴더 경로(Linux/macOS)'
  '이메일 주소'
  'IP 주소'
  '토큰이나 비밀 키'
)
patterns=(
  '[A-Za-z]:[\\/]+Users[\\/]+[^\\/ ]+'
  '/(home|Users)/[A-Za-z0-9._-]+'
  '[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}'
  '(^|[^0-9.])([0-9]{1,3}\.){3}[0-9]{1,3}([^0-9.]|$)'
  'gh[opsur]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY'
)
# 허용: 예시 도메인, GitHub noreply 주소, 루프백 주소
allowed='@example\.(com|org)|noreply@|@users\.noreply\.github\.com|127\.0\.0\.1|0\.0\.0\.0'

# 이 컴퓨터의 사용자 이름과 컴퓨터 이름(저장소에는 적지 않고 실행할 때 읽는다). CI에서는 건너뛴다.
local_words=()
if [ -z "${CI:-}" ]; then
  for word in "${USERNAME:-}" "${USER:-}" "${COMPUTERNAME:-}" "$(hostname 2>/dev/null)"; do
    [ "${#word}" -ge 4 ] && local_words+=("$word")
  done
fi

while IFS= read -r -d '' file; do
  if printf '%s' "$file" | grep -Eiq -e "$banned_files"; then
    report '올릴 수 없는 파일 종류' "$file"
    continue
  fi
  for i in "${!patterns[@]}"; do
    hit=$(show "$file" | grep -InE -e "${patterns[$i]}" | grep -Ev -e "$allowed" | head -n 3)
    [ -n "$hit" ] && report "${names[$i]}" "$file: $hit"
  done
  for word in ${local_words[@]+"${local_words[@]}"}; do
    hit=$(show "$file" | grep -InwiF -e "$word" | head -n 3)
    [ -n "$hit" ] && report '이 컴퓨터의 사용자 이름 또는 컴퓨터 이름' "$file: $hit"
  done
done < <(list)

# 커밋에 남는 이메일은 GitHub noreply 주소만 허용한다
if [ "$mode" != "--all" ]; then
  email=$(git var GIT_AUTHOR_IDENT | sed -E 's/.*<([^>]*)>.*/\1/')
  case "$email" in
    *@users.noreply.github.com) ;;
    *) report '커밋 작성자 이메일이 GitHub noreply 주소가 아님' \
         'git config user.email "<아이디>@users.noreply.github.com" 으로 바꾼 뒤 다시 커밋' ;;
  esac
fi

if [ "$fail" -ne 0 ]; then
  echo
  echo '개인정보·컴퓨터 정보로 보이는 내용이 있어 중단했다. 해당 내용을 지운 뒤 다시 시도한다.'
  exit 1
fi
