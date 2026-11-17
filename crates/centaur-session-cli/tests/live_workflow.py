#!/usr/bin/env python3
"""Opt-in real Centaur/Pi acceptance. Uses model tokens; never touches Kubernetes.

Requires a configured CLI, deployed terminal-workflow APIs, Pi provider credentials,
and persistent sandbox state. Leaves its two named sessions available for inspection.
This is not run by cargo test and does not claim to simulate a Pod restart.
"""
import argparse
import json
import subprocess
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", default="centaur")
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()

    def command(*parts):
        return subprocess.check_output([args.cli, "chat", *parts], text=True, timeout=45)

    def state(name):
        return json.loads(command("status", name))

    def wait(name, predicate):
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            result = state(name)
            execution = result.get("execution") or {}
            if execution.get("status") in ("failed", "cancelled"):
                raise AssertionError(f"{name}: {execution.get('status')}: {execution.get('error')}")
            if predicate(execution):
                return result
            time.sleep(1)
        raise AssertionError(f"{name}: timed out; inspect chat status/logs")

    run = uuid.uuid4().hex[:10]
    names = [f"accept-{run}-a", f"accept-{run}-b"]
    # Both sandboxes write the same pathname. Separate contents prove writable isolation.
    path = f"/tmp/centaur-isolation-{run}"
    readers = []
    try:
        for index, name in enumerate(names):
            marker = f"{run}-{index}"
            prompt = (
                "Use bash to run exactly this command, then reply DONE: "
                f"sleep 15; printf '%s\\n' '{marker}' > {path}; cat {path}"
            )
            command("new", "--harness", "pi", "--name", name, "--detach", "--message", prompt)
            readers.append(subprocess.Popen(
                [args.cli, "chat", "logs", name, "--follow"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            ))
        for name in names:
            wait(name, lambda execution: execution.get("status") == "running")
        assert all(state(name)["execution"]["status"] == "running" for name in names), "runs did not overlap"
        before = {name: state(name)["execution"]["execution_id"] for name in names}
        for reader in readers:
            reader.kill()
            reader.wait(timeout=10)
        # Killing both local stream consumers must not cancel or restart either task.
        for index, name in enumerate(names):
            completed = wait(name, lambda execution: execution.get("status") == "completed")
            assert completed["execution"]["execution_id"] == before[name]
            records = [json.loads(line) for line in command("logs", name).splitlines()]
            ids = [int(record["id"]) for record in records if record["id"]]
            assert ids == sorted(set(ids)), "duplicate/out-of-order replay IDs"
            marker = f"{run}-{index}"
            assert any(
                record["data"].get("params", {}).get("item", {}).get("aggregatedOutput", "").strip() == marker
                for record in records if isinstance(record["data"], dict)
            ), f"{name}: expected actual bash output, not just an assistant claim"
            cursor = completed["last_event_id"]
            command("send", name, f"Use bash to run cat {path}, then reply DONE.")
            continued = wait(name, lambda execution: execution.get("status") == "completed" and execution.get("execution_id") != before[name])
            assert continued["session"]["thread_key"] == completed["session"]["thread_key"]
            replay = [json.loads(line) for line in command("logs", name, "--after-event-id", str(cursor)).splitlines()]
            assert all(int(record["id"]) > cursor for record in replay if record["id"])
            assert any(
                record["data"].get("params", {}).get("item", {}).get("aggregatedOutput", "").strip() == marker
                for record in replay if isinstance(record["data"], dict)
            ), f"{name}: workspace did not survive disconnect / session continuation"
        listing = command("list")
        assert all(name in listing for name in names)
        print("PASS: concurrent independent Pi runs, killed clients, completion, replay, continuation and filesystem isolation")
        print("Not tested: physical laptop outage, provider token renewal, control-plane restart or Pod replacement")
    finally:
        for reader in readers:
            if reader.poll() is None:
                reader.kill()
                reader.wait(timeout=10)
        print("Sessions retained for inspection:", *names)


if __name__ == "__main__":
    main()
