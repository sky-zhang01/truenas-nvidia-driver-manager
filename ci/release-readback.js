#!/usr/bin/env node
// Read back one published tag and verify it resolves to the released image.
//
// usage: release-readback.js <manifest file> <tag ref> <image id> <digest>
//
// The digest is computed from the raw bytes the registry serves under the
// tag (a registry is content-addressed, so it is the sha256 of those bytes);
// docker manifest inspect cannot do this job — it never reports the index
// digest and drops descriptor annotations when it re-serializes. Accepted
// shapes: an OCI image index / Docker manifest list with exactly one
// linux/amd64 image manifest plus only attestation-manifest entries, the
// index digest equal to the image id; or, under a classic image store, a
// single image manifest whose config digest equals the image id. Anything
// else fails with the document it read.
"use strict";
const fs = require("node:fs");
const [file, ref, imageId, digest] = process.argv.slice(2);
const ATTESTATION = "attestation-manifest";
const IMAGE_MANIFESTS = [
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];
function fail(message) {
    console.error("release: " + ref + " " + message);
    console.error("release: the document this tag serves is:");
    console.error(fs.readFileSync(file, "utf8"));
    process.exit(1);
}
function isAttestation(entry) {
    return entry !== null && typeof entry === "object" &&
        entry.annotations !== null && typeof entry.annotations === "object" &&
        entry.annotations["vnd.docker.reference.type"] === ATTESTATION;
}
function platformOf(entry) {
    const platform = entry !== null && typeof entry === "object" ? entry.platform : null;
    if (platform === null || typeof platform !== "object" ||
        typeof platform.os !== "string" || typeof platform.architecture !== "string") {
        return null;
    }
    return platform.os + "/" + platform.architecture;
}
let doc;
try {
    doc = JSON.parse(fs.readFileSync(file, "utf8"));
} catch (error) {
    fail("did not come back as JSON: " + error.message);
}
if (doc !== null && typeof doc === "object" && Array.isArray(doc.manifests)) {
    // An OCI image index or a Docker manifest list: exactly one linux/amd64
    // image manifest, every other entry an attestation.
    const platforms = [];
    let image = null;
    for (const entry of doc.manifests) {
        if (isAttestation(entry)) {
            platforms.push("unknown/unknown (" + ATTESTATION + ")");
            continue;
        }
        const platform = platformOf(entry);
        const mediaType = entry !== null && typeof entry === "object" ? entry.mediaType : null;
        if (platform !== "linux/amd64" || !IMAGE_MANIFESTS.includes(mediaType)) {
            fail("carries an entry that is neither the linux/amd64 image manifest nor an attestation manifest: " + JSON.stringify(entry));
        }
        if (image !== null) {
            fail("carries more than one linux/amd64 image manifest: " + JSON.stringify(entry));
        }
        image = entry;
        platforms.push(platform);
    }
    if (image === null) {
        fail("carries no linux/amd64 image manifest");
    }
    if (digest !== imageId) {
        fail("resolves to the index " + digest + ", not the released image " + imageId);
    }
    console.log("image index; platforms: " + platforms.join(", "));
} else if (doc !== null && typeof doc === "object" &&
    doc.config !== null && typeof doc.config === "object" &&
    typeof doc.config.digest === "string" && doc.config.digest !== "") {
    // Classic image store: a single image manifest, whose identity is its
    // config digest.
    if (doc.config.digest !== imageId) {
        fail("points at config " + doc.config.digest + ", not the released image " + imageId);
    }
    console.log("single image manifest; config " + doc.config.digest);
} else {
    fail("resolves to neither an image index nor a single image manifest");
}
