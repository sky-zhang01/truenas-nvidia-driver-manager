#!/usr/bin/env bash
# Controls use synthetic values assembled at runtime to keep this file publishable.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
mkdir -p "${WORK}/tree/ci" "${WORK}/bin"
printf 'clean text\n' > "${WORK}/tree/clean.txt"
printf '[extend]\nuseDefault = true\n' > "${WORK}/tree/.gitleaks.toml"
printf '{"public_hosts":["registered.public-control","*.wildcard.public-control"],"entries":[]}\n' > "${WORK}/tree/ci/public-leak-allowlist.json"
PUBLIC_RELEASE_FORBIDDEN_HOSTS="$(printf '*.synthetic-%s.example.net' 'blocked')"
PUBLIC_RELEASE_FORBIDDEN_TERMS="$(printf 'synthetic-%s-marker' 'blocked')"
export PUBLIC_RELEASE_FORBIDDEN_HOSTS PUBLIC_RELEASE_FORBIDDEN_TERMS
export TEST_PRIVATE_OUTPUT="$PUBLIC_RELEASE_FORBIDDEN_TERMS"
export TEST_CALLS="${WORK}/calls"
cat > "${WORK}/bin/gitleaks" <<'STUB'
#!/usr/bin/env bash
if [ "${PUBLIC_RELEASE_FORBIDDEN_TERMS+x}" = x ] || [ "${PUBLIC_RELEASE_FORBIDDEN_HOSTS+x}" = x ]; then exit 90; fi
printf '%s\n' "$@" > "$TEST_CALLS"
printf '%s\n' "$TEST_PRIVATE_OUTPUT"
printf '%s\n' "$TEST_PRIVATE_OUTPUT" >&2
exit "${TEST_GITLEAKS_EXIT:-0}"
STUB
chmod +x "${WORK}/bin/gitleaks"
export PATH="${WORK}/bin:${PATH}"
CHECK="${SCRIPT_DIR}/public-leak-check.sh"
count=0
expect() {
    local want="$1" label="$2" status=0
    shift 2
    "$@" > "${WORK}/out" 2> "${WORK}/err" || status=$?
    if { [ "$want" = pass ] && [ "$status" -ne 0 ]; } || { [ "$want" = fail ] && [ "$status" -eq 0 ]; }; then
        printf 'FAIL: %s\n' "$label" >&2; exit 1
    fi
    if cat "${WORK}/out" "${WORK}/err" | grep -Fiq -e "$PUBLIC_RELEASE_FORBIDDEN_TERMS" -e "${PUBLIC_RELEASE_FORBIDDEN_HOSTS#*.}"; then
        printf 'FAIL: private input appeared in output\n' >&2; exit 1
    fi
    count=$((count + 1))
}
run() { bash "$CHECK" --root "${WORK}/tree" "$@"; }
expect pass clean run
[ -s "$TEST_CALLS" ]
grep -Fq 'dir' "$TEST_CALLS"
grep -Fq -- '--config' "$TEST_CALLS"
expect fail missing-policy env PUBLIC_RELEASE_FORBIDDEN_TERMS= bash "$CHECK" --root "${WORK}/tree"
expect fail missing-host-policy env -u PUBLIC_RELEASE_FORBIDDEN_HOSTS bash "$CHECK" --root "${WORK}/tree"
expect fail missing-both-policies env -u PUBLIC_RELEASE_FORBIDDEN_HOSTS -u PUBLIC_RELEASE_FORBIDDEN_TERMS bash "$CHECK" --root "${WORK}/tree"
expect pass explicit-generic-only env -u PUBLIC_RELEASE_FORBIDDEN_HOSTS -u PUBLIC_RELEASE_FORBIDDEN_TERMS bash "$CHECK" --root "${WORK}/tree" --generic-only
for value in ' ' 'bad..example' '*bad.example' 'https://bad.example' 'bad.example/path' 'ok.example,bad..example'; do
    expect fail invalid-host-policy env PUBLIC_RELEASE_FORBIDDEN_HOSTS="$value" bash "$CHECK" --root "${WORK}/tree"
    if [ "$value" != ' ' ] && grep -Fq -- "$value" "${WORK}/out" "${WORK}/err"; then printf 'FAIL: invalid host policy echoed\n' >&2; exit 1; fi
done
expect fail blank-terms-policy env PUBLIC_RELEASE_FORBIDDEN_TERMS=' ' bash "$CHECK" --root "${WORK}/tree"
for value in 'bad/value' 'bad@value' 'bad..value' 'valid,bad/value' '-bad'; do
    expect fail invalid-terms-policy env PUBLIC_RELEASE_FORBIDDEN_TERMS="$value" bash "$CHECK" --root "${WORK}/tree"
    if grep -Fq -- "$value" "${WORK}/out" "${WORK}/err"; then printf 'FAIL: invalid terms policy echoed\n' >&2; exit 1; fi
done
expect fail generic-only-supplied-policy run --generic-only
# All generic fixture addresses and identities below are synthetic, assembled at runtime.
check_payload() {
    local want="$1" label="$2" payload="$3"
    printf '%s\n' "$payload" > "${WORK}/tree/control"
    expect "$want" "$label-control-$((count + 1))" run
    rm "${WORK}/tree/control"
}
for address in "$(printf '10.%s' '20.30.40')" "$(printf '172.%s' '16.0.1')" "$(printf '192.%s' '168.1.2')" "$(printf '100.%s' '64.0.1')" "$(printf '169.%s' '254.1.2')" "$(printf '8.%s' '8.4.4')"; do
    check_payload fail default-deny-ipv4 "$address"
done
for address in '0.0.0.0' '127.0.0.1' '127.200.3.4' '255.255.255.255' '192.0.2.5' '198.51.100.8' '203.0.113.9'; do
    check_payload pass public-ipv4-exceptions "$address"
done
for address in "$(printf 'fc00:%s' ':1234')" "$(printf 'fd12:%s' '3456::a')" "$(printf 'fe80:%s' ':1')" "$(printf '2001:%s' '4860:4860::8888')" "$(printf '2606:%s' '4700:4700:0000:0000:0000:0000:1111')"; do
    check_payload fail default-deny-ipv6 "$address"
done
for address in '::' '::1' '2001:db8::a' '2001:0db8:0000:0000:0000:0000:0000:1' 'std::fs' 'crate::module::Type'; do
    check_payload pass public-ipv6-and-code-exceptions "$address"
done
for address in "$(printf '0000:0000:%s' '0000:0000:0000:0000:0000:0000')" "$(printf '0:0:%s' '0:0:0:0:0:1')"; do
    check_payload pass expanded-ipv6-local-exceptions "$address"
done
check_payload fail ipv4-trailing-punctuation "$(printf '8.%s.' '8.4.4')"
check_payload fail ipv6-trailing-punctuation "$(printf 'fc00:%s.' ':1')"
check_payload pass documented-mac "$(printf '00-00-%s' '5E-00-53-01')"
check_payload pass documented-mac-colons "$(printf '00:00:%s' '5e:00:53:ab')"
check_payload pass documented-mac-dotted "$(printf '0000.%s' '5e00.53ab')"
for suffix in local lan internal home.arpa localdomain intranet corp ts.net; do
    check_payload fail internal-dns "$(printf 'nas.%s' "$suffix")"
done
# The first DNS match is explicitly excepted; a later file must still be checked.
printf '%s\n' "$(printf 'synthetic-host.%s' 'internal')" > "${WORK}/tree/dns-allowed"
node - "${WORK}/tree" <<'JS'
const fs=require('fs'),c=require('crypto'),r=process.argv[2];
const line=fs.readFileSync(r+'/dns-allowed','utf8').trimEnd();
fs.writeFileSync(r+'/ci/public-leak-allowlist.json',JSON.stringify({public_hosts:['registered.public-control','*.wildcard.public-control'],entries:[{path:'dns-allowed',line_sha256:c.createHash('sha256').update(line).digest('hex'),rules:['internal-dns'],reason:'Synthetic lexical control.'}]}));
JS
expect pass exact-dns-line-exception run
cp "${WORK}/tree/dns-allowed" "${WORK}/tree/dns-denied"
expect fail internal-dns-cross-file-state run
rm "${WORK}/tree/dns-allowed" "${WORK}/tree/dns-denied"
printf '{"public_hosts":["registered.public-control","*.wildcard.public-control"],"entries":[]}\n' > "${WORK}/tree/ci/public-leak-allowlist.json"
unregistered_host="$(printf 'unregistered.%s' 'public-control')"
unregistered_ssh="git@${unregistered_host}"
for payload in "https://${unregistered_host}" "http://${unregistered_host}/path" "git@${unregistered_host}:project.git" "ssh://${unregistered_ssh}/project"; do
    check_payload fail unregistered-url-host "$payload"
done
# Reserved names need no public-host or email registration, including label boundaries.
for reserved_host in example invalid test localhost nas.example sub.example.invalid sub.example.test sub.example.localhost example.com example.net example.org child.example.com child.example.net child.example.org; do
    reserved_ssh="git@${reserved_host}"
    for payload in "https://${reserved_host}/path" "http://${reserved_host}:8080/path" "git@${reserved_host}:project.git" "ssh://${reserved_ssh}/project" "fixture@${reserved_host}"; do
        check_payload pass reserved-name "$payload"
    done
done
check_payload pass reserved-case-and-root-dot 'https://Sub.Example.NET./path'
check_payload pass reserved-email-case 'fixture@Sub.Example.NET'
for denied_host in example.com.outside example.net.outside example.org.outside notexample.com notexample.net notexample.org node.suffixexample node.suffixinvalid node.suffixtest node.suffixlocalhost example.public-control example.com-evil example.invalid-extra foo.example.test-bar example.com-123 example.net9 node.example.localhost-evil example.org_unsafe example.com-; do
    check_payload fail reserved-name-boundary "https://${denied_host}/path"
    check_payload fail reserved-email-boundary "fixture@${denied_host}"
done
# Reserved-name acceptance never overrides either private policy.
check_payload fail private-reserved-url "https://${PUBLIC_RELEASE_FORBIDDEN_HOSTS#*.}/path"
check_payload fail private-reserved-email "fixture@${PUBLIC_RELEASE_FORBIDDEN_HOSTS#*.}"
registered_host=registered.public-control
wildcard_host=wildcard.public-control
wildcard_child="sub.${wildcard_host}"
registered_ssh="git@${registered_host}"
for payload in "https://${registered_host}/path" "https://${wildcard_host}/path" "https://${wildcard_child}/path" "git@${registered_host}:project.git" "ssh://${registered_ssh}/project" 'https://example.org/path' 'https://sub.example.com/path' 'https://example.com' 'git@example.org:project.git' 'ssh://git@example.org/project' 'https://<nas-host>/path'; do
    check_payload pass registered-or-placeholder-host "$payload"
done
for payload in "$(printf '/%s/%s/data' 'Users' 'synthetic-person')" "$(printf '/%s/%s/data' 'home' 'synthetic-person')" "$(printf '%s:\\%s\\%s\\data' 'C' 'Users' 'synthetic-person')"; do
    check_payload fail personal-home-path "$payload"
done
for home_root in Users home; do
    check_payload fail personal-home-directory-root "$(printf '/%s/%s' "$home_root" 'synthetic-person')"
done
path_separator="$(printf '\134')"
for payload in "$(printf '/%s//%s/data' 'Users' 'synthetic-person')" "$(printf '/%s//%s/data' 'home' 'synthetic-person')" "C:${path_separator}Users${path_separator}${path_separator}synthetic-person${path_separator}data"; do
    check_payload fail repeated-personal-home-separator "$payload"
done
host="${PUBLIC_RELEASE_FORBIDDEN_HOSTS#*.}"
for payload in "$host" "child.$host" "${host//./%2e}" "${host//./\\u002e}" "${host//./\\x2e}" "${host//./\\056}" "${host//./[.]}" "${host//./(.)}" "${host//./ dot }" "${host//./\" + \".}" "${host//./.\" + \"}"; do
    check_payload fail private-host-encoding "$payload"
done
check_payload pass host-label-boundary "prefix${host}suffix"
for payload in "$PUBLIC_RELEASE_FORBIDDEN_TERMS" 'SYNTHETIC_BLOCKED_MARKER' 'Synthetic Blocked Marker' 'synthetic_blocked-marker'; do
    check_payload fail private-term-separators "$payload"
done
check_payload pass term-word-boundary "prefix${PUBLIC_RELEASE_FORBIDDEN_TERMS}suffix"
# Exercise whole-input private matching through the real scanner entry point.
# Keep fixtures outside the scanned tree until each individual control runs.
node - "${WORK}/split-controls" <<'JS'
const fs = require('node:fs');
const root = process.argv[2];
fs.mkdirSync(root);
const host = process.env.PUBLIC_RELEASE_FORBIDDEN_HOSTS.slice(2);
const term = process.env.PUBLIC_RELEASE_FORBIDDEN_TERMS;
const left = host.slice(0, 7), right = host.slice(7);
const termLeft = term.slice(0, 7), termRight = term.slice(7);
const lastDot = host.lastIndexOf('.');
const controls = [
  ['fail', 'host-complete-json-first-element', JSON.stringify([host, 'other']).replace(',', ', ')],
  ['fail', 'host-complete-json-compact-array', JSON.stringify([host, 'b.example.org'])],
  ['fail', 'term-complete-json-first-element', JSON.stringify([term, 'other']).replace(',', ', ')],
  ['fail', 'host-complete-adjacent-literal', 'x = ("' + host + '" "path")'],
  ['fail', 'host-complete-literal-suffix-join', 'x = "' + host + '" + "foo"'],
  ['fail', 'term-complete-literal-suffix-join', '"' + term + '" + "x"'],
  ['fail', 'host-complete-yaml-flow-list', '[' + host + ', other]'],
  ['fail', 'host-complete-toml-array', 'hosts = ["a.example.org", "' + host + '"]'],
  ['fail', 'host-complete-percent-dot-literal-join', '"' + host.replaceAll('.', '%2e') + '" + "suffix"'],
  ['fail', 'host-complete-bracket-dot-literal-join', '"' + host.replaceAll('.', '[.]') + '" + "suffix"'],
  ['fail', 'term-complete-literal-prefix-join', '"prefix" + "' + term + '"'],
  ['fail', 'host-complete-split-percent-dot-literal-join', '"' + host.replace('.', '%2\ne') + '" + "suffix"'],
  ['fail', 'host-complete-split-unicode-dot-literal-join', '"' + host.replace('.', '\\\nu002e') + '" + "suffix"'],
  ['fail', 'host-complete-split-dot-word-literal-join', '"' + host.replace('.', ' d\not ') + '" + "suffix"'],
  ['fail', 'host-sentence-label-wrap', 'The host is ' + host.slice(0, lastDot) + '\n' + host.slice(lastDot) + ' in the notes.'],
  ['fail', 'host-sentence-word-wrap', 'See ' + left + '\n' + right + ' for details.'],
  ['fail', 'host-sentence-hash-comments', '# configured ' + host.slice(0, lastDot) + '\n# ' + host.slice(lastDot)],
  ['fail', 'host-echo-separate-arguments', "echo '" + host.slice(0, lastDot) + "' '" + host.slice(lastDot) + "'"],
  ['fail', 'term-sentence-word-wrap', 'See ' + termLeft + '\n' + termRight + ' today.'],
  ['pass', 'host-original-suffix-boundary', host.slice(0, lastDot) + '\n' + host.slice(lastDot) + 'suffix'],
  ['pass', 'host-json-array-prefix-join', JSON.stringify(['prefix', left, right])],
  ['fail', 'host-complete-literal-prefix-join', '"prefix" +\n"' + host + '"'],
  ['fail', 'host-newline-between-labels', host.slice(0, lastDot) + '\n' + host.slice(lastDot)],
  ['fail', 'host-newline-inside-label', left + '\n  ' + right],
  ['fail', 'host-newline-percent-dot', host.replace('.', '%2e\n')],
  ['fail', 'host-newline-bracket-dot', host.replace('.', '[.]\n')],
  ['fail', 'host-split-percent-escape', host.replace('.', '%2\ne')],
  ['fail', 'host-split-unicode-escape-after-backslash', host.replace('.', '\\\nu002e')],
  ['fail', 'host-split-hex-escape-after-backslash', host.replace('.', '\\\nx2e')],
  ['fail', 'host-split-octal-escape-after-backslash', host.replace('.', '\\\n056')],
  ['fail', 'host-split-dot-word-after-d', host.replace('.', ' d\not ')],
  ['fail', 'host-split-dot-word-after-o', host.replace('.', ' do\nt ')],
  ['fail', 'host-split-unicode-escape', host.replace('.', '\\u00\n2e')],
  ['fail', 'host-split-hex-escape', host.replace('.', '\\x2\ne')],
  ['fail', 'host-split-octal-escape', host.replace('.', '\\05\n6')],
  ['fail', 'host-line-continuation', left + '\\\n\t' + right],
  ['fail', 'host-crlf-continuation', left + '\\\r\n\t' + right],
  ['fail', 'host-unicode-whitespace', left + '\u0085\u00a0\u2028\ufeff' + right],
  ['fail', 'host-line-comment', '// ' + left + '\n// ' + right],
  ['fail', 'host-hash-comment', '# ' + left + '\n# ' + right],
  ['fail', 'host-block-comment', '/*\n * ' + left + '\n * ' + right + '\n */'],
  ['fail', 'host-html-comment', '<!-- ' + left + '\n' + right + ' -->'],
  ['fail', 'host-html-comment-per-line', '<!-- ' + left + ' -->\n<!-- ' + right + ' -->'],
  ['fail', 'host-html-comment-between-parts', left + '<!-- note -->' + right],
  ['fail', 'host-html-comment-between-literals', '"' + left + '" <!-- note --> + "' + right + '"'],
  ['fail', 'host-html-comment-between-lines', left + '\n<!-- note -->\n' + right],
  ['fail', 'host-markdown', left + '\n' + right],
  ['fail', 'host-yaml-folded', 'host: >-\n  ' + left + '\n  ' + right],
  ['fail', 'host-yaml-indent-indicator', 'host: >2-\n  ' + left + '\n  ' + right],
  ['fail', 'host-yaml-header-comment', 'host: >- # hostname\n  ' + left + '\n  ' + right],
  ['fail', 'host-json-array', JSON.stringify([left, right], null, 2)],
  ['fail', 'host-rust-concat', 'concat!("' + left + '",\n "' + right + '")'],
  ['fail', 'host-rust-raw-concat', 'concat!(r#"' + left + '"#, r"' + right + '")'],
  ['fail', 'host-adjacent-literals', '"' + left + '"\n "' + right + '"'],
  ['fail', 'host-join-block-comment', '"' + left + '" /* note */ + "' + right + '"'],
  ['fail', 'host-join-line-comment', '"' + left + '" + // note\n "' + right + '"'],
  ['fail', 'host-python-raw-literals', 'r"' + left + '"\n r"' + right + '"'],
  ['fail', 'host-template', '`' + left + '${"' + right + '"}`'],
  ['fail', 'host-template-boundary', '`' + left + '${`' + right + '`}`'],
  ['fail', 'host-template-single-quoted-literal', '`' + left + "${'" + right + "'}`"],
  ['fail', 'host-empty-interpolation', left + '${}' + right],
  ['fail', 'host-shell-braced-variable', host.slice(0, lastDot) + '${empty}' + host.slice(lastDot)],
  ['fail', 'host-shell-unbraced-variable', '"' + host.slice(0, lastDot) + '"$empty"' + host.slice(lastDot) + '"'],
  ['fail', 'host-shell-append', 'host=' + host.slice(0, lastDot) + '\nhost+=' + host.slice(lastDot)],
  ['fail', 'host-shell-variable-inside-label', '"' + left + '"${empty}"' + right + '"'],
  ['fail', 'host-dot-rewrite', host.replaceAll('.', ' dot\n ')],
  ['fail', 'host-bracket-rewrite', host.replaceAll('.', '[\n.\t]')],
  ['fail', 'host-subdomain', 'child.' + left + '\n' + right],
  ['fail', 'term-newline-inside-word', termLeft + '\n\t' + termRight],
  ['fail', 'term-whitespace-separators', term.replaceAll('-', '\n  ')],
  ['fail', 'term-line-continuation', termLeft + '\\\n' + termRight],
  ['fail', 'term-line-comment', '// ' + termLeft + '\n// ' + termRight],
  ['fail', 'term-block-comment', '/* ' + termLeft + '\n * ' + termRight + ' */'],
  ['fail', 'term-html-comment', '<!-- ' + termLeft + '\n' + termRight + ' -->'],
  ['fail', 'term-html-comment-between-parts', termLeft + '<!-- note -->' + termRight],
  ['fail', 'term-yaml-folded', 'term: >-\n  ' + termLeft + '\n  ' + termRight],
  ['fail', 'term-json-array', JSON.stringify([termLeft, termRight])],
  ['fail', 'term-rust-concat', 'concat!("' + termLeft + '", "' + termRight + '")'],
  ['fail', 'term-rust-raw-concat', 'concat!(r#"' + termLeft + '"#, r"' + termRight + '")'],
  ['fail', 'term-adjacent-literals', '"' + termLeft + '"\n "' + termRight + '"'],
  ['fail', 'term-join-block-comment', '"' + termLeft + '" /* note */ + "' + termRight + '"'],
  ['fail', 'term-join-line-comment', '"' + termLeft + '" + // note\n "' + termRight + '"'],
  ['fail', 'term-template', '`' + termLeft + '${"' + termRight + '"}`'],
  ['fail', 'term-empty-interpolation', termLeft + '${}' + termRight],
  ['fail', 'term-shell-braced-variable', termLeft + '${empty}' + termRight],
  ['fail', 'term-shell-append', 'term=' + termLeft + '\nterm+=' + termRight],
  ['fail', 'term-case-and-separators', (termLeft + '\n' + termRight).replaceAll('-', '_').toUpperCase()],
  ['pass', 'host-flattened-prefix-boundary', 'prefix' + left + '\n' + right],
  ['pass', 'host-flattened-suffix-boundary', left + '\n' + right + 'suffix'],
  ['pass', 'host-flattened-hyphen-boundary', left + '\n' + right + '-extra'],
  ['pass', 'host-flattened-quote-boundary', '"prefix" + "' + left + '" + "' + right + '"'],
  ['pass', 'host-not-label-boundary', 'not' + host],
  ['pass', 'host-shell-prefix-boundary', 'prefix' + left + '${empty}' + right],
  ['pass', 'host-shell-suffix-boundary', left + '${empty}' + right + 'suffix'],
  ['pass', 'host-shell-hyphen-boundary', left + '${empty}' + right + '-extra'],
  ['pass', 'term-flattened-prefix-boundary', 'prefix' + termLeft + '\n' + termRight],
  ['pass', 'term-flattened-suffix-boundary', termLeft + '\n' + termRight + 'suffix'],
  ['pass', 'term-flattened-quote-boundary', '"' + termLeft + '" + "' + termRight + '" + "suffix"'],
  ['pass', 'term-shell-prefix-boundary', 'prefix' + termLeft + '${empty}' + termRight],
  ['pass', 'term-shell-suffix-boundary', termLeft + '${empty}' + termRight + 'suffix'],
  ['pass', 'generic-rules-are-not-flattened', '10.\n20.30.40\naa:bb:\ncc:dd:ee:ff\nnas.\ninternal'],
  ['pass', 'generic-rules-keep-shell-placeholders', '10.${empty}20.30.40\naa:bb:${empty}cc:dd:ee:ff\nnas.${empty}internal'],
  ['pass', 'ordinary-rust', 'use std::fs;\n1.81.0'],
  ['pass', 'ordinary-text', 'A normal Markdown paragraph.\nAnother ordinary line.'],
];
for (const join of [' + ', ' . ', ' .. ', ', ']) {
  for (const quote of ['"', "'", '`']) {
    controls.push(['fail', 'host-string-join', quote + left + quote + join + '\n' + quote + right + quote]);
    controls.push(['fail', 'term-string-join', quote + termLeft + quote + join + '\n' + quote + termRight + quote]);
  }
}
for (const [index, [want, label, text]] of controls.entries()) {
  fs.writeFileSync(root + '/' + index, text);
  fs.appendFileSync(root + '/cases', [want, label, index].join(' ') + '\n');
}
JS
while read -r want label fixture; do
    cp "${WORK}/split-controls/${fixture}" "${WORK}/tree/control"
    expect "$want" "$label" run
    if [ "$want" = fail ] && { [ -s "${WORK}/out" ] || ! [[ "$(cat "${WORK}/err")" =~ ^FAIL:\ file-id=[a-f0-9]{16}\ rule=forbidden$ ]]; }; then
        printf 'FAIL: %s diagnostics must contain only a file digest and forbidden rule\n' "$label" >&2
        exit 1
    fi
    rm "${WORK}/tree/control"
done < "${WORK}/split-controls/cases"
# Paths use the same flattened matcher, including directory and link payloads.
split_host="$(printf '%s\n%s' "${host:0:7}" "${host:7}")"
split_term="$(printf '%s\n%s' "${PUBLIC_RELEASE_FORBIDDEN_TERMS:0:7}" "${PUBLIC_RELEASE_FORBIDDEN_TERMS:7}")"
printf safe > "${WORK}/tree/${split_host}"
expect fail split-host-filename run
rm "${WORK}/tree/${split_host}"
mkdir "${WORK}/tree/${split_term}"
expect fail split-term-directory run
rmdir "${WORK}/tree/${split_term}"
printf safe > "${WORK}/tree/${split_term}"
ln -s "${split_term}" "${WORK}/tree/split-link"
expect fail split-term-link run
rm "${WORK}/tree/split-link" "${WORK}/tree/${split_term}"
check_payload pass documented-ipv6-is-not-a-mac "$(printf '2001:db8:aa:bb:%s' 'cc:dd:ee:ff')"
for delimiter in '-' '_' ' '; do
    check_payload fail term-surrounded-by-word-separators "prefix${delimiter}${PUBLIC_RELEASE_FORBIDDEN_TERMS}${delimiter}suffix"
done
# Commas and whitespace are both accepted policy separators.
expect pass multi-host-policy env PUBLIC_RELEASE_FORBIDDEN_HOSTS='first.example, second.example third.example' bash "$CHECK" --root "${WORK}/tree"
expect pass multi-term-policy env PUBLIC_RELEASE_FORBIDDEN_TERMS='one-marker, two-marker three-marker' bash "$CHECK" --root "${WORK}/tree"
check_payload fail private-host-filename-content "$host"
printf 'safe' > "${WORK}/tree/$host"
expect fail private-host-filename run
rm "${WORK}/tree/$host"
mkdir "${WORK}/tree/SYNTHETIC_BLOCKED_MARKER"
expect fail private-term-directory run
rmdir "${WORK}/tree/SYNTHETIC_BLOCKED_MARKER"
# Network exceptions bind one exact line, never an entire file.
printf 'release=%s\n' "$(printf '8.%s' '8.4.4')" > "${WORK}/tree/version"
node - "${WORK}/tree" <<'JS'
const fs=require('fs'),c=require('crypto'),r=process.argv[2];
const line=fs.readFileSync(r+'/version','utf8').trimEnd();
fs.writeFileSync(r+'/ci/public-leak-allowlist.json',JSON.stringify({public_hosts:['registered.public-control','*.wildcard.public-control'],entries:[{path:'version',line_sha256:c.createHash('sha256').update(line).digest('hex'),rules:['ipv4'],reason:'Synthetic version control.'}]}));
JS
expect pass exact-network-line-exception run
printf 'other=%s\n' "$(printf '9.%s' '9.9.9')" >> "${WORK}/tree/version"
expect fail adjacent-network-line-not-exempted run
printf 'release=%s\n' "$(printf '8.%s' '8.4.5')" > "${WORK}/tree/version"
expect fail network-line-hash-drift run
node - "${WORK}/tree" <<'JS'
const fs=require('fs'),c=require('crypto'),r=process.argv[2];
fs.writeFileSync(r+'/ci/public-leak-allowlist.json',JSON.stringify({public_hosts:[],entries:[{path:'version',sha256:c.createHash('sha256').update(fs.readFileSync(r+'/version')).digest('hex'),rules:['ipv4'],reason:'Broad network exceptions must fail.'}]}));
JS
expect fail whole-file-network-exception run
rm "${WORK}/tree/version"
for rule in ipv6 mac; do
    if [ "$rule" = ipv6 ]; then payload="$(printf 'fd00:%s' ':55')"; else payload="$(printf 'aa:bb:%s' 'cc:dd:ee:ff')"; fi
    if [ "$rule" = mac ]; then payload='clean schema control'; fi
    printf '%s\n' "$payload" > "${WORK}/tree/network"
    node - "${WORK}/tree" "$rule" <<'JS'
const fs=require('fs'),c=require('crypto'),r=process.argv[2],rule=process.argv[3];
const line=fs.readFileSync(r+'/network','utf8').trimEnd();
fs.writeFileSync(r+'/ci/public-leak-allowlist.json',JSON.stringify({public_hosts:[],entries:[{path:'network',line_sha256:c.createHash('sha256').update(line).digest('hex'),rules:[rule],reason:'Synthetic network line control.'}]}));
JS
    if [ "$rule" = mac ]; then
        expect fail mac-line-exception-schema run
        rm "${WORK}/tree/network"
        continue
    fi
    expect pass exact-network-line-exception run
    printf 'changed=' >> "${WORK}/tree/network"
    printf '%s\n' "$payload" >> "${WORK}/tree/network"
    expect fail network-exception-other-line run
    rm "${WORK}/tree/network"
done
printf '{"public_hosts":["registered.public-control","*.wildcard.public-control"],"entries":[]}\n' > "${WORK}/tree/ci/public-leak-allowlist.json"
printf '%s' "$PUBLIC_RELEASE_FORBIDDEN_TERMS" > "${WORK}/tree/bad"
expect fail forbidden run
rm "${WORK}/tree/bad"
for payload in "$(printf '10.%s' '20.30.40')" "$(printf 'aa:bb:%s' 'cc:dd:ee:ff')" "$(printf '/%s/%s/data' 'mnt' 'actual-pool')" "$(printf 'person@%s' 'unregistered.public-control')" "$(printf 'orches%s' 'trator')"; do
    printf '%s' "$payload" > "${WORK}/tree/bad"
    expect fail generic-rule run
    rm "${WORK}/tree/bad"
done
printf '\344\270\255\346\226\207' > "${WORK}/tree/bad"
expect fail chinese run
mv "${WORK}/tree/bad" "${WORK}/tree/doc.zh-CN.md"
expect pass localized-doc run
rm "${WORK}/tree/doc.zh-CN.md"
# Chinese exceptions bind a single complete line and its exact file.
printf 'English text\n\344\270\255\346\226\207 language link\n' > "${WORK}/tree/language.md"
node - "${WORK}/tree" <<'JS'
const fs=require('fs'),c=require('crypto'),r=process.argv[2];
const line=fs.readFileSync(r+'/language.md','utf8').split('\n')[1];
fs.writeFileSync(r+'/ci/public-leak-allowlist.json',JSON.stringify({public_hosts:[],entries:[{path:'language.md',line_sha256:c.createHash('sha256').update(line).digest('hex'),rules:['chinese'],reason:'Synthetic localized language link.'}]}));
JS
expect pass exact-chinese-line-exception run
printf 'More English text\n' >> "${WORK}/tree/language.md"
expect pass chinese-line-independent-of-other-lines run
cp "${WORK}/tree/language.md" "${WORK}/language-original"
printf '\344\270\255\346\226\207 elsewhere\n' >> "${WORK}/tree/language.md"
expect fail chinese-exception-other-line run
cp "${WORK}/language-original" "${WORK}/tree/language.md"
printf 'changed ' > "${WORK}/tree/language.md"
cat "${WORK}/language-original" >> "${WORK}/tree/language.md"
# Changing an English line preserves the exact registered Chinese line.
expect pass chinese-line-independent-of-english-edit run
printf 'English text\nchanged \344\270\255\346\226\207 language link\n' > "${WORK}/tree/language.md"
expect fail chinese-exception-line-drift run
cp "${WORK}/language-original" "${WORK}/tree/language.md"
mv "${WORK}/tree/language.md" "${WORK}/tree/other.md"
expect fail chinese-exception-path-drift run
rm "${WORK}/tree/other.md"
printf '{"public_hosts":["registered.public-control","*.wildcard.public-control"],"entries":[]}\n' > "${WORK}/tree/ci/public-leak-allowlist.json"
# Variable-looking values are literal path fixtures.
# shellcheck disable=SC2016
for pool in "$(printf '\344\270\255\346\226\207')" 'synthetic pool' '$POOL' '${POOL}' '%encoded'; do
    printf '/%s/%s/data' 'mnt' "$pool" > "${WORK}/tree/storage.zh-CN.md"
    expect fail non-placeholder-mount run
done
for pool in synthetic "$(printf '\344\270\255\346\226\207')"; do
    printf '/%s///%s/data' 'mnt' "$pool" > "${WORK}/tree/storage.zh-CN.md"
    expect fail repeated-mount-separator run
done
mount_root="$(printf '/%s/' 'mnt')"
for payload in "${mount_root}<pool>/data" "${mount_root}//<pool>/data" "\"${mount_root}<pool>/data\"" "${mount_root}" "\"${mount_root}\"" "${mount_root} path description"; do
    printf '%s' "$payload" > "${WORK}/tree/storage.zh-CN.md"
    expect pass mount-boundary run
done
rm "${WORK}/tree/storage.zh-CN.md"
printf '%s' "$PUBLIC_RELEASE_FORBIDDEN_TERMS" > "${WORK}/tree/doc.zh-CN.md"
expect fail localized-doc-policy run
rm "${WORK}/tree/doc.zh-CN.md"
printf 'safe' > "${WORK}/tree/${PUBLIC_RELEASE_FORBIDDEN_TERMS}"
expect fail private-filename run
rm "${WORK}/tree/${PUBLIC_RELEASE_FORBIDDEN_TERMS}"
printf '<svg><text>%s</text></svg>' "$PUBLIC_RELEASE_FORBIDDEN_TERMS" > "${WORK}/tree/brand.svg"
expect fail svg-text run
rm "${WORK}/tree/brand.svg"
printf 'public+fixture@%s' 'users.noreply.github.com' > "${WORK}/tree/noreply"
expect pass github-noreply run
rm "${WORK}/tree/noreply"
ln -s "${WORK}/out" "${WORK}/tree/outside"
expect fail external-link run
rm "${WORK}/tree/outside"
: > "${WORK}/tree/empty"
expect pass empty-text run
rm "${WORK}/tree/empty"
node - "${WORK}/tree/large-source.txt" <<'JS'
require('fs').writeFileSync(process.argv[2], 'const value = 1;\n'.repeat(60000));
JS
expect pass large-text-format-window run
rm "${WORK}/tree/large-source.txt"
for artifact in sample.pdf sample.zip sample.exe sample.wasm no-extension-pdf no-extension-archive renamed.txt offset.txt; do
    case "$artifact" in
        no-extension-pdf|sample.pdf) printf '%%PDF-1.4\nSynthetic document\n' > "${WORK}/tree/${artifact}" ;;
        no-extension-archive) printf '!<arch>\nSynthetic archive\n' > "${WORK}/tree/${artifact}" ;;
        renamed.txt) printf 'GIF89a0000ppp;' > "${WORK}/tree/${artifact}" ;;
        offset.txt) printf 'padding\n%%PDF-1.4\nSynthetic document\n' > "${WORK}/tree/${artifact}" ;;
        *) printf 'Synthetic ASCII payload\n' > "${WORK}/tree/${artifact}" ;;
    esac
    expect fail unregistered-format run
    node - "${WORK}/tree" "$artifact" <<'JS'
const fs = require('fs'), c = require('crypto'), root = process.argv[2], file = process.argv[3];
fs.writeFileSync(root + '/ci/public-leak-allowlist.json', JSON.stringify({public_hosts:['registered.public-control','*.wildcard.public-control'],entries:[{path:file,sha256:c.createHash('sha256').update(fs.readFileSync(root+'/'+file)).digest('hex'),rules:['binary'],reason:'Synthetic file format control.'}]}));
JS
    expect pass registered-format run
    printf 'changed' >> "${WORK}/tree/${artifact}"
    expect fail format-hash-drift run
    rm "${WORK}/tree/${artifact}"
done
printf '{"public_hosts":["registered.public-control","*.wildcard.public-control"],"entries":[]}\n' > "${WORK}/tree/ci/public-leak-allowlist.json"
printf '\000\001\002' > "${WORK}/tree/bad.bin"
expect fail binary run
node - "${WORK}/tree" <<'JS'
const fs = require('fs'), c = require('crypto'), root = process.argv[2];
fs.writeFileSync(root + '/ci/public-leak-allowlist.json', JSON.stringify({public_hosts:['registered.public-control','*.wildcard.public-control'],entries:[{path:'bad.bin',sha256:c.createHash('sha256').update(fs.readFileSync(root+'/bad.bin')).digest('hex'),rules:['binary'],reason:'Synthetic binary control.'}]}));
JS
expect pass registered-binary run
printf '\003' >> "${WORK}/tree/bad.bin"
expect fail binary-hash-drift run
printf '\000\001\002' > "${WORK}/tree/bad.bin"
printf '%s' "$PUBLIC_RELEASE_FORBIDDEN_TERMS" >> "${WORK}/tree/bad.bin"
expect fail forbidden-binary run
rm "${WORK}/tree/bad.bin"
printf '%s' "$PUBLIC_RELEASE_FORBIDDEN_TERMS" > "${WORK}/tree/excepted"
node - "${WORK}/tree" <<'JS'
const fs = require('fs'), c = require('crypto'), root = process.argv[2];
fs.writeFileSync(root + '/ci/public-leak-allowlist.json', JSON.stringify({public_hosts:['registered.public-control','*.wildcard.public-control'],entries:[{path:'excepted',sha256:c.createHash('sha256').update(fs.readFileSync(root+'/excepted')).digest('hex'),rules:['email','chinese','process'],reason:'Policy values cannot be exempted.'}]}));
JS
expect fail forbidden-cannot-be-exempted run
rm "${WORK}/tree/excepted"
printf '{"public_hosts":["registered.public-control","*.wildcard.public-control"],"entries":[]}\n' > "${WORK}/tree/ci/public-leak-allowlist.json"
expect fail scanner-failure env TEST_GITLEAKS_EXIT=1 bash "$CHECK" --root "${WORK}/tree"
expect fail scanner-unavailable env PATH="/usr/bin:/bin" bash "$CHECK" --root "${WORK}/tree"
expect fail content-only-in-ci env CI=true bash "$CHECK" --root "${WORK}/tree" --content-only
expect pass explicit-content-only env -u CI -u GITHUB_ACTIONS -u GITEA_ACTIONS bash "$CHECK" --root "${WORK}/tree" --content-only
mkdir "${WORK}/only-node"
ln -s "$(command -v node)" "${WORK}/only-node/node"
expect fail format-tool-missing env PATH="${WORK}/only-node" /bin/bash "$CHECK" --root "${WORK}/tree"
cat > "${WORK}/bin/file" <<'STUB'
#!/usr/bin/env bash
if [ "${PUBLIC_RELEASE_FORBIDDEN_TERMS+x}" = x ] || [ "${PUBLIC_RELEASE_FORBIDDEN_HOSTS+x}" = x ]; then exit 90; fi
cat > /dev/null
printf '%s\n' "$TEST_PRIVATE_OUTPUT" >&2
case "${TEST_FILE_MODE:-failure}" in
    valid) printf 'text/x-Algol68\n' ;;
    invalid) printf '%s\n' "$TEST_PRIVATE_OUTPUT" ;;
    *) printf '%s\n' "$TEST_PRIVATE_OUTPUT"; exit 1 ;;
esac
STUB
chmod +x "${WORK}/bin/file"
expect fail format-tool-failure run
expect fail invalid-format-output env TEST_FILE_MODE=invalid bash "$CHECK" --root "${WORK}/tree"
expect pass format-tool-private-environment env TEST_FILE_MODE=valid bash "$CHECK" --root "${WORK}/tree"
rm "${WORK}/bin/file"
printf '{"entries":[{"path":"../escape"}]}\n' > "${WORK}/tree/ci/public-leak-allowlist.json"
expect fail invalid-policy run
printf 'PASS: %s privacy controls (gitleaks is a test stub).\n' "$count"
# Check every real registration even if a generic detector no longer hits it.
node - "${SCRIPT_DIR}/.." <<'JS'
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');
const root = process.argv[2], policy = JSON.parse(fs.readFileSync(path.join(root, 'ci/public-leak-allowlist.json')));
const hash = data => crypto.createHash('sha256').update(data).digest('hex');
for (const entry of policy.entries) {
  const data = fs.readFileSync(path.join(root, entry.path));
  if (entry.line_sha256 ? !data.toString('utf8').split(/\r?\n/).some(line => hash(line) === entry.line_sha256) : hash(data) !== entry.sha256) {
    console.error('FAIL: stale public allowlist registration');
    process.exit(1);
  }
}
console.log('PASS: ' + policy.entries.length + ' current public allowlist hashes.');
JS
