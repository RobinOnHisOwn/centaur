#!/usr/bin/env python3
"""Black-box configuration/auth tests for the built binary; no real credentials.
Run with CENTAUR_CLI_TEST_BINARY pointing to the executable.
"""
import base64
import http.server
import json
import os
import pathlib
import pty
import select
import shutil
import subprocess
import tempfile
import threading
import time
import termios
import unittest
import urllib.parse


class TokenServer(http.server.BaseHTTPRequestHandler):
    requests = 0
    reject = False
    lifecycle = []
    prune_policy = True
    prune_error = 0
    prune_page_error = False
    mcp_items = []
    execute_error = False
    create_ok = False
    list_pages = False
    list_page_error = False

    def lifecycle_response(self, value):
        assert self.headers['Authorization'] == 'Bearer fixture-access-token'
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

    def do_DELETE(self):
        path = urllib.parse.unquote(self.path)
        if path == '/api/mcp/servers/docs':
            type(self).mcp_items[0].update(status='not_connected', available=False)
            self.lifecycle_response({'disconnected': True, 'provider_token_revoked': False})
            return
        if path.endswith('/prune'):
            assert path in ['/api/session/api:acl_test:old/prune', '/api/session/api:acl_test:old2/prune']
            type(self).lifecycle.append(('DELETE', path))
            if path.endswith('old2/prune') and type(self).prune_error:
                self.send_response(type(self).prune_error)
                self.end_headers()
                self.wfile.write(b'changed or cleanup failed')
            else:
                self.lifecycle_response({'deleted': path.endswith('old/prune')})
            return
        assert path == '/api/session/api:acl_test:old'
        type(self).lifecycle.append(('DELETE', path))
        self.lifecycle_response({'deleted': True})

    def do_GET(self):
        if self.path == '/api/mcp/servers':
            self.lifecycle_response({'items': type(self).mcp_items})
            return
        type(self).lifecycle.append(('GET', self.path))
        if '/events?' in self.path:
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            self.wfile.write(b'id: 1\nevent: session.execution_completed\ndata: {}\n\n')
            self.wfile.flush()
            self.stream_stop.wait(15)
            return
        if self.path.endswith('/state'):
            self.lifecycle_response({'session': {'thread_key': 'api:acl_test:old'},
                                     'execution': {'execution_id': 'exe_fixture', 'status': 'queued'},
                                     'messages': [], 'last_event_id': 0})
            return
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
        if query.get('prunable_only') == ['true']:
            second = bool(query.get('after_thread_key', [''])[0])
            if second and type(self).prune_page_error:
                self.send_response(500)
                self.end_headers()
                return
            name = 'old2' if second else 'old'
            value = {'items': [{'thread_key': f'api:acl_test:{name}', 'status': 'failed',
                                'archived': second, 'updated_at': '2020-01-01T00:00:00Z'}],
                     'next_after_thread_key': None if second else 'api:acl_test:old'}
            if type(self).prune_policy:
                value['prune_older_than_days'] = 30
            self.lifecycle_response(value)
            return
        if self.list_pages:
            second = bool(query.get('after_thread_key'))
            if second and self.list_page_error:
                self.send_response(503)
                self.end_headers()
                return
            name = 'old2' if second else 'old'
            self.lifecycle_response({'items': [{'thread_key': f'api:acl_test:{name}'}],
                                     'next_after_thread_key': None if second else 'api:acl_test:old'})
            return
        self.lifecycle_response({'items': [], 'next_after_thread_key': None})

    def do_POST(self):
        path = urllib.parse.unquote(self.path)
        if path.startswith('/api/session/') and not path.endswith('/archive'):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            type(self).lifecycle.append(('POST', path, body))
            if path.endswith('/messages'):
                self.lifecycle_response({'ok': True, 'message_ids': ['msg_fixture']})
            elif path.endswith('/execute'):
                if self.execute_error:
                    self.send_response(503)
                    self.end_headers()
                    self.wfile.write(b'enqueue unavailable')
                else:
                    self.lifecycle_response({'ok': True, 'execution_id': 'exe_fixture',
                                             'thread_key': 'api:acl_test:old', 'status': 'queued'})
            elif self.create_ok:
                self.lifecycle_response({'thread_key': path.removeprefix('/api/session/'),
                                         'harness_type': body['harness_type'], 'status': 'idle',
                                         'created_at': [2026, 324, 0, 0, 0, 0, 0, 0, 0],
                                         'updated_at': [2026, 324, 0, 0, 0, 0, 0, 0, 0]})
            else:
                self.send_response(400)
                self.end_headers()
                self.wfile.write(b'{"error":"principal is required: this API client has no default principal","ok":false}')
            return
        if self.path.startswith('/api/mcp/servers'):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if self.path == '/api/mcp/servers':
                assert body == {'name': 'docs', 'url': 'https://mcp.example.com/mcp'}
                self.lifecycle_response({'ok': True, 'authorization_url': 'https://console.example.com/oauth/docs/start'})
            else:
                assert self.path == '/api/mcp/servers/docs/login'
                self.lifecycle_response({'authorization_url': 'https://console.example.com/oauth/docs/start'})
            return
        if self.path.endswith('/archive'):
            path = urllib.parse.unquote(self.path)
            assert path == '/api/session/api:acl_test:old/archive'
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            type(self).lifecycle.append(('POST', path, body))
            self.lifecycle_response(body)
            return
        type(self).requests += 1
        assert self.path == "/oauth/token"
        expected = base64.b64encode(b"acl_test:fixture-secret").decode()
        assert self.headers["Authorization"] == "Basic " + expected
        body = self.rfile.read(int(self.headers["Content-Length"]))
        assert body == b"grant_type=client_credentials"
        self.send_response(401 if self.reject else 200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({
            "error": "fixture-secret" if self.reject else None,
            "access_token": "fixture-access-token",
            "expires_in": 3600,
        }).encode())

    def log_message(self, *_args):
        pass


class Configuration(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="centaur-cli-test-", dir="/tmp")
        self.addCleanup(self.directory.cleanup)
        self.config = pathlib.Path(self.directory.name) / "config.json"
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("CENTAUR_")}
        self.env["CENTAUR_CONFIG"] = str(self.config)
        self.binary = os.environ["CENTAUR_CLI_TEST_BINARY"]
        TokenServer.requests = 0
        TokenServer.reject = False
        TokenServer.lifecycle = []
        TokenServer.prune_policy = True
        TokenServer.prune_error = 0
        TokenServer.prune_page_error = False
        TokenServer.mcp_items = []
        TokenServer.execute_error = False
        TokenServer.create_ok = False
        TokenServer.list_pages = False
        TokenServer.list_page_error = False
        TokenServer.stream_stop = threading.Event()
        self.addCleanup(TokenServer.stream_stop.set)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), TokenServer)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}"
        self.run_cli("config", "set", "endpoint", self.endpoint)
        self.run_cli("config", "set", "client-id", "acl_test")

    def run_cli(self, *args, input=None, ok=True):
        result = subprocess.run([self.binary, *args], env=self.env, input=input,
                                capture_output=True, text=True, timeout=15)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("fixture-secret", result.stdout + result.stderr)
        self.assertNotIn("fixture-access-token", result.stdout + result.stderr)
        return result

    def test_legacy_flags_cannot_be_silently_ignored_by_subcommands(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        for flags in [
            ['--harness-type', 'pi'], ['--message', 'hello'], ['--thread-key', 'cli:test'],
            ['--attach'], ['--input-line', '{}'], ['--idle-timeout-ms', '1000'],
            ['--max-duration-ms', '60000'], ['--after-event-id', '0'], ['--all-events'],
            ['--exit-on-terminal'], ['--exit-on-output-type', 'done'], ['--stdin-events'],
            ['--tui'], ['--debug'],
        ]:
            with self.subTest(flags=flags):
                before = TokenServer.requests
                result = self.run_cli(*flags, 'chat', 'list', ok=False)
                self.assertIn('legacy', result.stderr.lower())
                self.assertEqual(TokenServer.requests, before)
                self.assertEqual(TokenServer.lifecycle, [])
        self.run_cli('--api-url', self.endpoint, 'chat', 'list')

    def test_legacy_execution_flags_still_select_the_harness_and_duration(self):
        TokenServer.create_ok = True
        self.env['CENTAUR_API_TOKEN'] = 'fixture-access-token'
        self.run_cli('--api-url', self.endpoint, '--thread-key', 'api:acl_test:legacy',
                     '--harness-type', 'pi', '--message', 'legacy prompt',
                     '--max-duration-ms', '3210', '--exit-on-terminal')
        posts = [row for row in TokenServer.lifecycle if row[0] == 'POST']
        self.assertEqual(posts[0][2]['harness_type'], 'pi')
        self.assertEqual(posts[1][2]['messages'][0]['parts'][0]['text'], 'legacy prompt')
        self.assertEqual(posts[2][2]['max_duration_ms'], 3210)

    def test_config_inspection_unset_and_local_logout(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        self.run_cli('config', 'set', 'principal', 'prn_example')
        shown = self.run_cli('config', 'show').stdout
        self.assertIn(self.endpoint, shown)
        self.assertIn('prn_example', shown)
        self.assertIn('present', shown)
        self.env['CENTAUR_CLIENT_ID'] = 'acl_override'
        self.assertIn('CENTAUR_CLIENT_ID', self.run_cli('config', 'show').stdout)
        del self.env['CENTAUR_CLIENT_ID']
        self.run_cli('config', 'unset', 'principal')
        self.assertNotIn('principal', json.loads(self.config.read_text()))
        self.run_cli('logout')
        saved = json.loads(self.config.read_text())
        self.assertEqual(saved['endpoint'], self.endpoint)
        self.assertFalse(set(saved) & {'client-secret', 'credential-endpoint', 'credential-client-id'})
        self.run_cli('logout')
        self.env['CENTAUR_CLIENT_SECRET'] = 'fixture-secret'
        self.assertIn('environment', self.run_cli('logout').stdout.lower())
        self.run_cli('chat', 'list')  # logout must not claim to revoke environment credentials
        self.run_cli('config', 'unset', 'endpoint')
        self.assertNotIn('endpoint', json.loads(self.config.read_text()))

    def test_config_show_rejects_insecure_or_malformed_secret_files_without_leaking(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        self.config.chmod(0o644)
        self.assertIn('0600', self.run_cli('config', 'show', ok=False).stderr)
        self.config.chmod(0o600)
        self.config.write_text('{"client-secret": fixture-secret}')
        self.run_cli('config', 'show', ok=False)

    def test_readiness_is_not_implied_by_login_or_listing(self):
        result = self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        self.assertIn('readiness', result.stdout.lower())
        self.assertEqual(TokenServer.lifecycle, [])
        result = self.run_cli('chat', 'new', '--name', 'test', '--detach', ok=False)
        self.assertIn('Console', result.stderr)
        self.assertIn('default principal', result.stderr)

    def test_integration_aliases_reuse_console_and_warn_about_add(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        self.assertEqual(self.run_cli('integrations', 'list').stdout,
                         self.run_cli('mcp', 'list').stdout)
        self.assertEqual(self.run_cli('integrations', 'connect', 'docs').stdout,
                         self.run_cli('mcp', 'login', 'docs').stdout)
        result = self.run_cli('mcp', 'add', 'docs', '--url', 'https://mcp.example.com/mcp')
        self.assertIn('deprecated', result.stderr.lower())

    def test_attached_tui_sends_edited_unicode_with_selected_timeout_and_detaches(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        master, slave = pty.openpty()
        termios.tcsetwinsize(slave, (24, 100))
        process = subprocess.Popen(
            [self.binary, 'chat', 'attach', 'old', '--max-duration-ms', '1234'],
            env={**self.env, 'TERM': 'xterm-256color'},
            pass_fds=(slave,), preexec_fn=lambda: os.login_tty(slave),
        )
        os.close(slave)
        output = bytearray()
        cursor_reports = 0

        def wait_for(predicate):
            nonlocal cursor_reports
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if predicate():
                    return
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        output.extend(os.read(master, 65536))
                        # A PTY is not a terminal emulator: answer Crossterm's DSR query.
                        reports = output.count(b'\x1b[6n')
                        if reports > cursor_reports:
                            os.write(master, b'\x1b[1;1R' * (reports - cursor_reports))
                            cursor_reports = reports
                    except OSError:
                        break
            self.fail('TUI condition not reached: ' + output.decode(errors='replace')[-2000:])

        try:
            wait_for(lambda: b'CENTAUR' in output)
            # Edit in the middle of multibyte text, then submit exactly what is displayed.
            os.write(master, 'a界🙂z'.encode() + b'\x1b[D\x7f' + 'é'.encode() + b'\x1b[3~\r')
            wait_for(lambda: any(row[0] == 'POST' and row[1].endswith('/execute') for row in TokenServer.lifecycle))
            posts = [row for row in TokenServer.lifecycle if row[0] == 'POST']
            self.assertEqual(posts[0][2]['messages'][0]['parts'][0]['text'], 'a界é')
            self.assertEqual(posts[1][2]['max_duration_ms'], 1234)
            # Force a full frame: differential rendering may omit unchanged letters
            # within the receipt, so raw PTY bytes are not a contiguous screen string.
            termios.tcsetwinsize(master, (24, 120))
            wait_for(lambda: b'exe_fixture' in output)
            os.write(master, b'\x04')
            self.assertEqual(process.wait(timeout=10), 0)
            self.assertEqual(len([row for row in TokenServer.lifecycle if row[0] == 'POST']), 2,
                             'detaching must not submit an interrupt or another turn')
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            os.close(master)
            TokenServer.stream_stop.set()

    def test_json_new_receipt_and_list_pagination(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        TokenServer.create_ok = True
        result = self.run_cli('chat', 'new', '--name', 'old', '--harness', 'pi',
                              '--message', 'hello', '--detach', '--json')
        self.assertEqual(json.loads(result.stdout)['execution_id'], 'exe_fixture')
        self.assertEqual(TokenServer.lifecycle[0][2]['harness_type'], 'pi')
        self.assertEqual(len(TokenServer.lifecycle), 3)
        TokenServer.list_pages = True
        result = self.run_cli('chat', 'list', '--json')
        self.assertEqual([item['thread_key'] for item in json.loads(result.stdout)['items']],
                         ['api:acl_test:old', 'api:acl_test:old2'])
        TokenServer.list_page_error = True
        result = self.run_cli('chat', 'list', '--json', ok=False)
        self.assertEqual(result.stdout, '', 'do not print partial JSON on pagination failure')

    def test_invalid_new_inputs_do_not_create_sessions(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        TokenServer.create_ok = True
        for options in [['--message', '  '], ['--max-duration-ms', '0']]:
            with self.subTest(options=options):
                TokenServer.lifecycle = []
                self.run_cli('chat', 'new', '--name', 'old', '--detach', *options, ok=False)
                self.assertEqual(TokenServer.lifecycle, [])

    def test_send_exposes_execution_receipt_and_preserves_request(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        result = self.run_cli('chat', 'send', 'old', 'hello', '--request-id', 'retry-1',
                              '--max-duration-ms', '1234', '--json')
        self.assertEqual(json.loads(result.stdout), {
            'ok': True, 'execution_id': 'exe_fixture', 'thread_key': 'api:acl_test:old',
            'status': 'queued', 'request_id': 'retry-1'})
        self.assertIn('retry-1', result.stderr)
        append, execute = TokenServer.lifecycle
        self.assertEqual(append[2]['messages'][0]['client_message_id'], 'retry-1')
        self.assertEqual(execute[2]['idempotency_key'], 'retry-1')
        self.assertEqual(execute[2]['max_duration_ms'], 1234)
        result = self.run_cli('chat', 'send', 'old', 'hello')
        self.assertIn('exe_fixture', result.stdout)
        self.assertIn('queued', result.stdout)
        TokenServer.execute_error = True
        result = self.run_cli('chat', 'send', 'old', 'hello', '--json', ok=False)
        self.assertEqual(result.stdout, '')
        self.assertIn('prompt is persisted', result.stderr)

    def test_json_reads_and_attach_timeout_help(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        self.assertEqual(json.loads(self.run_cli('chat', 'list', '--json').stdout), {'items': []})
        status = json.loads(self.run_cli('chat', 'status', 'old', '--json').stdout)
        self.assertEqual(status['execution']['execution_id'], 'exe_fixture')
        self.assertIn('--max-duration-ms', self.run_cli('chat', 'attach', '--help').stdout)
        self.assertIn('reuse', self.run_cli('chat', 'new', '--help').stdout)
        self.assertIn('enqueue', self.run_cli('chat', 'send', '--help').stdout.lower())
        self.assertIn('not a complete', self.run_cli('chat', 'history', '--help').stdout.lower())
        self.run_cli('chat', 'new', '--json', ok=False)  # JSON must not start a TUI

    @unittest.skipUnless(shutil.which("strace"), "requires strace")
    def test_atomic_config_replacement_uses_destination_directory(self):
        trace = pathlib.Path(self.directory.name) / "rename.log"
        result = subprocess.run(
            ["strace", "-e", "trace=rename", "-o", str(trace), self.binary,
             "config", "set", "endpoint", self.endpoint],
            env=self.env, capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        renames = [line for line in trace.read_text().splitlines() if line.startswith("rename(")]
        self.assertEqual(len(renames), 1)
        source = json.loads(renames[0].split("(", 1)[1].split(", ", 1)[0])
        self.assertEqual(pathlib.Path(source).parent, self.config.parent)
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)

    def test_personal_mcp_reuses_native_connections_without_local_configuration(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        before = self.config.read_bytes()
        native = {'id': 'oap_docs', 'name': 'docs', 'url': 'https://mcp.example.com/mcp',
                  'status': 'connected', 'available': True}
        TokenServer.mcp_items = [native.copy()]
        self.assertIn('docs', self.run_cli('mcp', 'list').stdout)
        result = self.run_cli('mcp', 'add', 'docs', '--url', native['url'])
        self.assertIn('No separate MCP configuration record', result.stdout)
        self.assertEqual(TokenServer.mcp_items, [native], 'add must not create another registry entry')
        self.assertIn('docs', self.run_cli('mcp', 'list').stdout)
        self.assertIn('https://console.example.com/oauth/docs/start', self.run_cli('mcp', 'login', 'docs').stdout)
        count = TokenServer.requests
        self.run_cli('mcp', 'remove', 'docs', '--yes', ok=False)
        self.assertEqual(count, TokenServer.requests)
        self.assertEqual(TokenServer.mcp_items, [native])
        self.assertEqual(before, self.config.read_bytes(), 'MCP settings/tokens must not be stored locally')
        count = TokenServer.requests
        self.run_cli('mcp', 'add', 'bad', '--url', 'https://user:password@example.com/mcp', ok=False)
        self.assertEqual(count, TokenServer.requests, 'reject unsafe MCP URLs before authenticating')

    def test_login_is_verified_private_and_bound_to_endpoint(self):
        self.run_cli("login", "--secret-stdin", input="fixture-secret\n")
        self.assertEqual(TokenServer.requests, 1)
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
        saved = json.loads(self.config.read_text())
        self.assertEqual(saved["credential-endpoint"], self.endpoint)
        self.env["CENTAUR_API_URL"] = "http://127.0.0.1:1"
        result = self.run_cli("chat", "list", ok=False)
        self.assertIn("different endpoint", result.stderr)
        self.assertEqual(TokenServer.requests, 1)
        del self.env["CENTAUR_API_URL"]
        self.run_cli("config", "set", "client-id", "acl_different")
        self.assertNotIn("client-secret", json.loads(self.config.read_text()))

    def test_rejected_login_never_saves_or_prints_credentials(self):
        TokenServer.reject = True
        self.run_cli("login", "--secret-stdin", input="fixture-secret\n", ok=False)
        self.assertNotIn("client-secret", json.loads(self.config.read_text()))

    def test_environment_secret_does_not_force_an_interactive_prompt(self):
        self.env["CENTAUR_CLIENT_SECRET"] = "fixture-secret"
        self.run_cli("login", input="")
        self.assertEqual(TokenServer.requests, 1)

    def test_delete_requires_confirmation_and_calls_real_delete_endpoint(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        result = self.run_cli('chat', 'delete', 'old', ok=False)
        self.assertIn('--yes', result.stderr)
        self.assertEqual(TokenServer.lifecycle, [])
        self.run_cli('chat', 'delete', 'old', '--yes')
        self.assertEqual(TokenServer.lifecycle, [('DELETE', '/api/session/api:acl_test:old')])

    def test_archive_restore_and_inclusive_listing(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        self.run_cli('chat', 'archive', 'old')
        self.run_cli('chat', 'unarchive', 'old')
        self.run_cli('chat', 'list', '--all')
        self.assertEqual(TokenServer.lifecycle[:2], [
            ('POST', '/api/session/api:acl_test:old/archive', {'archived': True}),
            ('POST', '/api/session/api:acl_test:old/archive', {'archived': False}),
        ])
        self.assertIn('include_archived=true', TokenServer.lifecycle[2][1])

    def test_prune_previews_all_pages_then_uses_only_guarded_deletion(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        preview = self.run_cli('chat', 'prune')
        self.assertIn('matched=2', preview.stdout)
        self.assertTrue(all(entry[0] == 'GET' for entry in TokenServer.lifecycle))
        TokenServer.lifecycle = []
        result = self.run_cli('chat', 'prune', '--yes')
        self.assertIn('deleted=1', result.stdout)
        self.assertIn('skipped=1', result.stdout)
        deletes = [entry[1] for entry in TokenServer.lifecycle if entry[0] == 'DELETE']
        self.assertEqual(deletes, ['/api/session/api:acl_test:old/prune', '/api/session/api:acl_test:old2/prune'])

    def test_prune_refuses_a_backend_without_the_age_filter_contract(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        TokenServer.prune_policy = False
        self.run_cli('chat', 'prune', '--yes', ok=False)
        self.assertTrue(all(entry[0] == 'GET' for entry in TokenServer.lifecycle))

    def test_prune_reports_partial_failures_and_skips_conflicts(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        for status in [409, 500]:
            with self.subTest(status=status):
                TokenServer.lifecycle = []
                TokenServer.prune_error = status
                result = self.run_cli('chat', 'prune', '--yes', ok=status == 409)
                self.assertIn('deleted=1', result.stdout)
                self.assertIn('skipped=1' if status == 409 else 'failed=1', result.stdout)
                self.assertEqual(sum(entry[0] == 'DELETE' for entry in TokenServer.lifecycle), 2)

    def test_prune_reports_progress_when_a_later_page_fails(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        TokenServer.prune_page_error = True
        result = self.run_cli('chat', 'prune', '--yes', ok=False)
        self.assertIn('deleted=1', result.stdout)
        self.assertIn('failed=1', result.stdout)
        self.assertEqual(sum(entry[0] == 'DELETE' for entry in TokenServer.lifecycle), 1)

    def test_insecure_remote_endpoint_is_rejected(self):
        self.run_cli("config", "set", "endpoint", "http://remote.example", ok=False)
        self.assertEqual(json.loads(self.config.read_text())["endpoint"], self.endpoint)


if __name__ == "__main__":
    unittest.main()
