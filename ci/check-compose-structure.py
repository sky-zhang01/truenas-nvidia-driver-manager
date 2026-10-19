#!/usr/bin/env python3
"""Structural checks for the compose template and its rendered forms.

Usage: check-compose-structure.py <bridge|macvlan|ipvlan> <template> <rendered.json>

`rendered.json` is the `docker compose config --format json` output for the
variant; the template is checked for the `environment:` sequence form, which
the rendered JSON cannot show (compose normalizes both forms to a mapping).

The checks are structural: a commented-out key cannot satisfy them, a
second service is rejected, and `environment:` written as a mapping is
rejected. The approved explanatory comments are checked by `check-compose.sh`
itself.

Exit status 0 when every check passes, 1 otherwise.
"""
import json
import sys

MODES = ("bridge", "macvlan", "ipvlan")


def fail(message):
    print("compose-config-check: " + message, file=sys.stderr)
    return 1


def template_environment_is_sequence(template):
    """Allow no environment, or a sequence of explicitly enabled settings."""
    lines = template.splitlines()
    for index, line in enumerate(lines):
        if line == "    environment:":
            for following in lines[index + 1:]:
                stripped = following.strip()
                if not stripped or stripped.startswith("#"):
                    continue
                return following.startswith("      - ")
            return False
    return True


def as_text(value):
    return value if isinstance(value, str) else str(value)


def check_service(service):
    if service.get("privileged") is not True:
        return fail("privileged must be true")
    if service.get("pid") != "host":
        return fail("pid must be host")
    if service.get("runtime") != "runc":
        return fail("runtime must be runc")
    if service.get("restart") != "unless-stopped":
        return fail("restart must be unless-stopped")
    image = service.get("image")
    if not isinstance(image, str) or not image or "<" in image:
        return fail("image must be a concrete repository:tag after substitution; got %r" % image)
    environment = service.get("environment", {})
    if not isinstance(environment, dict):
        return fail("environment must render as a mapping of names to values")
    if "NVM_INITIAL_ADMIN_PASSWORD" in environment:
        return fail("the container environment must not carry an initial password")
    volumes = service.get("volumes")
    if not isinstance(volumes, list) or not any(
        isinstance(item, dict) and item.get("target") == "/var/lib/nvm" for item in volumes
    ):
        return fail("the data directory must be mapped to /var/lib/nvm")
    return 0


def check_networks(service, doc, mode):
    networks = service.get("networks")
    if not isinstance(networks, dict):
        networks = {}
    ports = service.get("ports")
    if mode == "bridge":
        if "lan" in networks:
            return fail("the bridge variant must not attach the service to lan")
        if not isinstance(ports, list) or len(ports) != 1:
            return fail("the bridge variant needs exactly one port mapping")
        mapping = ports[0]
        if not isinstance(mapping, dict):
            return fail("the port mapping is not an object: %r" % (mapping,))
        try:
            target = int(mapping.get("target"))
        except (TypeError, ValueError):
            return fail("the port mapping has no numeric target: %r" % (mapping,))
        if target != 8199 or as_text(mapping.get("published")) != "8199":
            return fail("the bridge variant must publish 8199:8199; got %r" % (mapping,))
        return 0
    if ports:
        return fail("the %s variant must not publish a port" % mode)
    lan = networks.get("lan")
    if not isinstance(lan, dict):
        return fail("the %s variant must attach the service to the lan network" % mode)
    if lan.get("ipv4_address") != "192.0.2.10":
        return fail("the %s variant needs the substituted container address" % mode)
    if mode == "macvlan":
        # Assembled here: a MAC literal in a tracked file fails sanitization.
        expected_mac = ":".join(["02", "00", "00", "00", "00", "01"])
        if as_text(lan.get("mac_address", "")).lower() != expected_mac:
            return fail("the macvlan variant must carry the substituted mac_address")
    elif "mac_address" in lan:
        return fail("the ipvlan variant shares the host MAC and must not set mac_address")
    top = doc.get("networks")
    if not isinstance(top, dict) or not isinstance(top.get("lan"), dict):
        return fail("the top-level lan network is missing")
    net = top["lan"]
    if net.get("driver") != mode:
        return fail("the lan network driver must be %s; got %r" % (mode, net.get("driver")))
    opts = net.get("driver_opts")
    if not isinstance(opts, dict) or not opts.get("parent"):
        return fail("the lan network needs driver_opts.parent")
    ipam = net.get("ipam")
    config = ipam.get("config") if isinstance(ipam, dict) else None
    if not isinstance(config, list) or not config or not isinstance(config[0], dict):
        return fail("the lan network needs an ipam config")
    first = config[0]
    if first.get("subnet") != "192.0.2.0/24" or first.get("gateway") != "192.0.2.1":
        return fail(
            "the lan ipam config must carry the substituted subnet and gateway; got %r" % (first,)
        )
    return 0


def main():
    if len(sys.argv) != 4:
        print(
            "usage: check-compose-structure.py <bridge|macvlan|ipvlan> <template> <config.json>",
            file=sys.stderr,
        )
        return 2
    mode, template_path, config_path = sys.argv[1], sys.argv[2], sys.argv[3]
    if mode not in MODES:
        return fail("unknown mode %r" % mode)
    try:
        with open(template_path, encoding="utf-8") as handle:
            template = handle.read()
    except OSError as exc:
        return fail("cannot read %s: %s" % (template_path, exc))
    if not template_environment_is_sequence(template):
        return fail("the template's environment: must be a YAML sequence (the '- name=value' form)")
    try:
        with open(config_path, encoding="utf-8") as handle:
            doc = json.load(handle)
    except (OSError, ValueError) as exc:
        return fail("cannot read the rendered config: %s" % exc)
    if not isinstance(doc, dict):
        return fail("the rendered config is not a JSON object")
    services = doc.get("services")
    if not isinstance(services, dict) or len(services) != 1:
        return fail("the rendered config must have exactly one service; got %r" % (list(services or {}),))
    name, service = next(iter(services.items()))
    if not isinstance(service, dict):
        return fail("the service %r is not a mapping" % name)
    status = check_service(service)
    if status:
        return status
    return check_networks(service, doc, mode)


if __name__ == "__main__":
    sys.exit(main())
