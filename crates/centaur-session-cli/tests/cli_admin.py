#!/usr/bin/env python3
"""Loopback contract tests; run with CENTAUR_CLI_TEST_BINARY set. No real keys."""
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import unittest
import urllib.parse

KEY = 'iak_' + 'a' * 64
CANARY = 'never-print-this-response-secret'


class Console(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, body=None, **headers):
        self.send_response(status)
        for key, value in headers.items():
            self.send_header(key, value)
        self.end_headers()
        if body is not None:
            self.wfile.write(json.dumps(body).encode())

    def handle_request(self):
        parsed = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        raw = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        body = json.loads(raw) if raw else None
        self.server.calls.append((self.command, parsed.path, query, body))
        if self.headers.get('Authorization') != 'Bearer ' + KEY:
            self.reply(401, {'error': {'message': CANARY}})
            return
        mode = self.server.mode
        if mode == 'redirect':
            self.reply(302, Location=self.server.url + '/credential-trap')
            return
        if mode == 'drop' and self.command != 'GET':
            self.connection.shutdown(socket.SHUT_RDWR)
            self.connection.close()
            return
        if mode == 'error' or (mode == 'page-error' and query.get('page') == ['2']):
            self.reply(403, {'error': {'message': CANARY, 'secret': KEY}})
            return
        if mode == 'no-content':
            self.reply(204)
            return
        if mode == 'object-list':
            self.reply(200, {'data': {}})
            return
        if mode == 'malformed':
            self.reply(200, {'wrong': CANARY})
            return
        if mode == 'large':
            self.reply(200, {'data': 'x' * (2 * 1024 * 1024)})
            return
        path = parsed.path.removeprefix('/api/v1/')
        parts = path.split('/')
        resource = parts[0]
        prefix = {'principals': 'prn', 'roles': 'role', 'grants': 'grant'}.get(resource)
        if prefix is None:
            self.reply(404)
            return
        oid = prefix + '_fixture'
        if 'missing' in parts:
            self.reply(404, {'error': {'message': CANARY}})
            return
        if self.command == 'DELETE':
            self.reply(204)
            return
        if path.endswith('/effective_config'):
            self.reply(200, {'data': {'id': oid, 'config': '[redacted]'}})
            return
        if self.command == 'GET' and (len(parts) == 1 or parts[-1] in ['roles', 'grants']):
            page = int(query.get('page', ['1'])[0])
            item_prefix = 'role' if parts[-1] == 'roles' else 'grant' if parts[-1] == 'grants' else prefix
            meta = {'page': page, 'limit': 1, 'total': 2, 'total_pages': 2}
            if mode == 'bad-pagination':
                meta['page'] = 0
            if mode == 'no-meta':
                meta = None
            response = {'data': [{'id': item_prefix + '_' + str(page)}]}
            # Principal role assignments are genuinely unpaginated.
            if not (len(parts) > 1 and parts[-1] == 'roles'):
                if meta is not None:
                    response['meta'] = meta
            self.reply(200, response)
            return
        if self.command == 'POST' and parts[-1] == 'roles' and len(parts) > 1:
            self.reply(201, {'data': {'id': body['data']['role_id']}})
            return
        self.reply(201 if self.command in ['POST', 'PUT'] else 200,
                   {'data': {'id': oid, 'foreign_id': 'fixture'}})

    do_GET = handle_request
    do_POST = handle_request
    do_PUT = handle_request
    do_PATCH = handle_request
    do_DELETE = handle_request


class AdminTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = os.environ['CENTAUR_CLI_TEST_BINARY']
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Console)
        cls.server.daemon_threads = True
        cls.server.url = f'http://127.0.0.1:{cls.server.server_port}'
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='centaur-admin-', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.config = Path(self.tmp.name) / 'config.json'
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('CENTAUR_')}
        self.env.update(CENTAUR_CONFIG=str(self.config), CENTAUR_ADMIN_URL=self.server.url,
                        CENTAUR_ADMIN_API_KEY=KEY)
        self.server.calls = []
        self.server.mode = ''

    def cli(self, *args, input=None, success=True, env=None):
        result = subprocess.run([self.binary, *args], input=input, text=True,
                                capture_output=True, env=env or self.env, timeout=12)
        self.assertEqual(result.returncode == 0, success, result.stderr)
        self.assertNotIn(KEY, result.stdout + result.stderr)
        self.assertNotIn(CANARY, result.stdout + result.stderr)
        return result

    def admin(self, *args, **kwargs):
        return self.cli('admin', *args, **kwargs)

    def document(self, data):
        return json.dumps({'data': data})

    def test_help_and_gated_commands(self):
        help_text = self.admin('--help').stdout
        for group in ['principals', 'roles', 'grants', 'login', 'logout', 'status', 'config']:
            self.assertIn(group, help_text)
        for args in [('users', 'list'), ('api-clients', 'list'), ('principals', 'delete', 'prn_x')]:
            self.admin(*args, success=False)
        self.assertEqual(self.server.calls, [])

    def test_login_config_binding_and_independent_logout(self):
        env = self.env.copy()
        del env['CENTAUR_ADMIN_API_KEY']
        self.cli('config', 'set', 'client-id', 'acl_chat', env=env)
        self.admin('config', 'set', 'endpoint', self.server.url, env=env)
        self.admin('login', '--key-stdin', input=KEY + '\n', env=env)
        saved = json.loads(self.config.read_text())
        self.assertEqual(saved['admin-api-key'], KEY)
        self.assertEqual(saved['client-id'], 'acl_chat')
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.server.calls[0][:3], ('GET', '/api/v1/roles', {'limit': ['1'], 'page': ['1']}))
        self.admin('config', 'show', env=env)
        self.admin('status', '--json', env=env)
        self.cli('logout', env=env)
        self.assertEqual(json.loads(self.config.read_text())['admin-api-key'], KEY)
        saved = json.loads(self.config.read_text())
        saved.update({'client-secret': 'synthetic-chat-secret', 'credential-endpoint': 'https://chat.example',
                      'credential-client-id': 'acl_chat'})
        self.config.write_text(json.dumps(saved))
        self.admin('logout', env=env)
        saved = json.loads(self.config.read_text())
        self.assertNotIn('admin-api-key', saved)
        self.assertEqual(saved['client-secret'], 'synthetic-chat-secret')

    def test_login_and_status_require_an_actual_roles_listing(self):
        for mode in ['no-content', 'object-list']:
            self.server.mode = mode
            self.admin('login', '--key-stdin', input=KEY, success=False)
            self.assertFalse(self.config.exists())
            self.admin('status', success=False)

    def test_oversized_secret_is_rejected_instead_of_silently_truncated(self):
        self.admin('login', '--key-stdin', input=KEY + '\n' * 17_000, success=False)
        self.assertFalse(self.config.exists())
        self.assertEqual(self.server.calls, [])

    def test_changed_endpoint_never_reuses_saved_key(self):
        env = self.env.copy()
        del env['CENTAUR_ADMIN_API_KEY']
        self.admin('login', '--key-stdin', input=KEY, env=env)
        changed = dict(env, CENTAUR_ADMIN_URL=self.server.url + '/other')
        before = len(self.server.calls)
        self.admin('status', success=False, env=changed)
        self.assertEqual(len(self.server.calls), before)
        self.admin('config', 'set', 'endpoint', 'https://console.example', env=env)
        self.assertNotIn('admin-api-key', json.loads(self.config.read_text()))
        self.admin('config', 'unset', 'endpoint', env=env)
        self.assertNotIn('admin-endpoint', json.loads(self.config.read_text()))

    def test_permissions_and_rejected_login(self):
        self.config.write_text(json.dumps({'admin-api-key': KEY}))
        self.config.chmod(0o644)
        self.admin('config', 'show', success=False)
        self.assertEqual(self.server.calls, [])
        self.config.unlink()
        self.server.mode = 'error'
        self.admin('login', '--key-stdin', input=KEY, success=False)
        self.assertFalse(self.config.exists())

    def test_chat_credentials_and_flags_are_not_admin_credentials(self):
        env = dict(self.env, CENTAUR_API_URL='https://chat.example', CENTAUR_API_TOKEN='chat-token')
        self.admin('status', env=env)
        del env['CENTAUR_ADMIN_API_KEY']
        before = len(self.server.calls)
        self.admin('status', success=False, env=env)
        self.assertEqual(len(self.server.calls), before)
        self.cli('--api-url', self.server.url, 'admin', 'status', success=False)
        self.cli('--api-token', 'synthetic-token', 'admin', 'status', success=False)
        self.assertEqual(len(self.server.calls), before)

    def test_lists_aggregate_console_pagination_and_filters(self):
        for resource in ['principals', 'roles']:
            with self.subTest(resource=resource):
                value = json.loads(self.admin(resource, 'list', '--label', 'team=a b', '--limit', '1', '--json').stdout)
                self.assertEqual(len(value['items']), 2)
                self.assertEqual(self.server.calls[-1][2], {'page': ['2'], 'limit': ['1'], 'labels[team]': ['a b']})
        for mode in ['page-error', 'bad-pagination', 'malformed', 'large', 'no-meta']:
            self.server.mode = mode
            result = self.admin('principals', 'list', '--json', success=False)
            self.assertEqual(result.stdout, '')

    def test_get_lookup_effective_config_and_roles(self):
        for resource, prefix in [('principals', 'prn'), ('roles', 'role')]:
            for ref, route in [(prefix + '_fixture', prefix + '_fixture'), ('fixture', 'lookup/fixture')]:
                value = json.loads(self.admin(resource, 'get', ref, '--json').stdout)
                self.assertEqual(value['data']['id'], prefix + '_fixture')
                self.assertEqual(self.server.calls[-1][1], '/api/v1/' + resource + '/' + route)
        self.admin('principals', 'effective-config', 'fixture', '--json')
        self.assertEqual(self.server.calls[-1][1], '/api/v1/principals/lookup/fixture/effective_config')
        value = json.loads(self.admin('principals', 'roles', 'list', 'fixture', '--json').stdout)
        self.assertEqual(value, {'items': [{'id': 'role_1'}]})
        self.admin('principals', 'get', 'missing', success=False)
        self.admin('roles', 'get', 'missing', success=False)

    def test_dotted_references_are_encoded_before_rails_format_recognition(self):
        for resource in ['principals', 'roles']:
            self.admin(resource, 'get', 'team.ops')
            self.assertEqual(self.server.calls[-1][1], f'/api/v1/{resource}/lookup/team%2Eops')
            self.admin(resource, 'upsert', 'team.ops', '--file', '-', input=self.document({'name': 'Example'}))
            self.assertEqual(self.server.calls[-1][1], f'/api/v1/{resource}/team%2Eops')
        self.admin('principals', 'effective-config', 'team.ops')
        self.assertEqual(self.server.calls[-1][1], '/api/v1/principals/lookup/team%2Eops/effective_config')

    def test_create_update_upsert_are_distinct(self):
        for resource, prefix in [('principals', 'prn'), ('roles', 'role')]:
            for action, args, verb, suffix in [
                ('create', [], 'POST', ''),
                ('update', ['fixture'], 'PATCH', '/' + prefix + '_fixture'),
                ('upsert', ['fixture'], 'PUT', '/fixture'),
            ]:
                with self.subTest(resource=resource, action=action):
                    data = {'name': 'Example', 'labels': {'team': 'ops'}}
                    self.admin(resource, action, *args, '--file', '-', '--json', input=self.document(data))
                    self.assertEqual(self.server.calls[-1], (verb, '/api/v1/' + resource + suffix, {}, {'data': data}))
            before = len(self.server.calls)
            self.admin(resource, 'update', 'missing', '--file', '-', input=self.document({'name': 'x'}), success=False)
            self.assertEqual(len(self.server.calls), before + 1)
            self.assertEqual(self.server.calls[-1][0], 'GET')
        doc = Path(self.tmp.name) / 'role.json'
        doc.write_text(self.document({'foreign_id': 'fixture', 'name': 'Example'}))
        self.admin('roles', 'create', '--file', str(doc))

    def test_assignments_and_confirmed_deletion(self):
        self.admin('principals', 'roles', 'add', 'fixture', 'fixture', '--json')
        self.assertEqual(self.server.calls[-1], ('POST', '/api/v1/principals/prn_fixture/roles', {},
                                                {'data': {'role_id': 'role_fixture'}}))
        for args in [('principals', 'roles', 'remove', 'fixture', 'fixture'),
                     ('roles', 'delete', 'fixture'), ('grants', 'delete', 'grant_fixture')]:
            before = len(self.server.calls)
            self.admin(*args, success=False)
            self.assertEqual(len(self.server.calls), before)
            self.assertEqual(json.loads(self.admin(*args, '--yes', '--json').stdout), {'ok': True})
            self.assertEqual(self.server.calls[-1][0], 'DELETE')

    def test_grant_routes_and_all_secret_kinds(self):
        for flag, resource in [('--principal', 'principals'), ('--role', 'roles')]:
            value = json.loads(self.admin('grants', 'list', flag, 'fixture', '--json').stdout)
            self.assertEqual(len(value['items']), 2)
            self.assertIn('/' + resource + '/', self.server.calls[-1][1])
        self.admin('grants', 'get', 'grant_fixture', '--json')
        for key, prefix in [('static_secret_id', 'ssr'), ('gcp_auth_secret_id', 'gas'),
                            ('gcp_id_token_secret_id', 'gid'), ('aws_auth_secret_id', 'aas'),
                            ('oauth_token_secret_id', 'ots'), ('pg_dsn_secret_id', 'pgs'),
                            ('hmac_secret_id', 'hms'), ('ssh_key_secret_id', 'sks')]:
            data = {'principal_id': 'prn_fixture', key: prefix + '_fixture'}
            self.admin('grants', 'create', '--file', '-', '--json', input=self.document(data))
            self.assertEqual(self.server.calls[-1][3], {'data': data})

    def test_bad_input_sends_no_requests(self):
        for args, data in [
            (['principals', 'create'], {'kind': 'console_user'}),
            (['principals', 'create'], {'console_user_id': 1}),
            (['principals', 'create'], {'slack_email': 'someone@example.com'}),
            (['principals', 'create'], {'name': {'wrong': 'shape'}}),
            (['principals', 'create'], {'sandbox_observability_enabled': 'true'}),
            (['roles', 'update', 'fixture'], {'foreign_id': 'fixture'}),
            (['roles', 'upsert', 'fixture'], {'foreign_id': 'different'}),
            (['roles', 'create'], {'assign_by_default': True}),
            (['grants', 'create'], {'principal_id': 'prn_fixture', 'role_id': 'role_fixture', 'static_secret_id': 'ssr_fixture'}),
            (['grants', 'create'], {'role_id': 'role_fixture', 'static_secret_id': 'ssr_fixture', 'hmac_secret_id': 'hms_fixture'}),
            (['grants', 'create'], {'principal_id': 'prn_fixture'}),
        ]:
            with self.subTest(args=args, data=data):
                self.admin(*args, '--file', '-', input=self.document(data), success=False)
        self.admin('roles', 'create', '--file', '-', input='{broken', success=False)
        self.admin('roles', 'create', '--file', '-', input=json.dumps({'data': {}, 'secret': CANARY}), success=False)
        self.admin('principals', 'get', '../roles', success=False)
        self.admin('roles', 'get', '..', success=False)
        self.admin('roles', 'upsert', 'role_fixture', '--file', '-', input=self.document({'name': 'x'}), success=False)
        self.admin('roles', 'list', '--label', 'broken', success=False)
        self.admin('roles', 'list', '--limit', '0', success=False)
        self.admin('grants', 'list', success=False)
        self.admin('grants', 'list', '--principal', 'fixture', '--role', 'fixture', success=False)
        self.assertEqual(self.server.calls, [])

    def test_redirects_errors_and_ambiguous_writes_are_not_retried(self):
        self.server.mode = 'redirect'
        self.admin('status', success=False)
        self.assertEqual(len(self.server.calls), 1)
        self.server.mode = 'drop'
        self.server.calls = []
        result = self.admin('roles', 'create', '--file', '-', input=self.document({'name': 'x'}), success=False)
        self.assertIn('uncertain', result.stderr.lower())
        self.assertEqual(len(self.server.calls), 1)
        for endpoint in ['http://remote.example', 'https://user:password@example.com', 'https://example.com/?key=value']:
            env = dict(self.env, CENTAUR_ADMIN_URL=endpoint)
            self.admin('status', env=env, success=False)
        self.assertEqual(len(self.server.calls), 1)


if __name__ == '__main__':
    unittest.main()
