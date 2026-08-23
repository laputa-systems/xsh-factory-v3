use factory_protocol::{
    AssignmentPacketWireV2, canonical_assignment_packet_json_v2, parse_assignment_packet_v2,
    unsigned_assignment_packet_digest_v2,
};

#[test]
fn print_fixture_unsigned_digest() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v2.json").trim_end();
    let packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    let digest = unsigned_assignment_packet_digest_v2(&packet).expect("unsigned digest");
    println!("{digest}");
    assert_eq!(
        canonical_assignment_packet_json_v2(&packet).expect("canonical fixture"),
        raw
    );
}

#[test]
fn sealed_fixture_round_trips_and_rejects_noncanonical_bytes() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v2.json").trim_end();
    let packet = parse_assignment_packet_v2(raw.as_bytes()).expect("sealed fixture");
    assert_eq!(
        packet.packet_digest,
        "b23f71d5acd39a09da0fe58373c5c6918f1e04d3c1bbcf84901b8c16cdaf1778"
    );
    assert!(parse_assignment_packet_v2(format!(" {raw}").as_bytes()).is_err());
    assert!(
        parse_assignment_packet_v2(
            raw.replace(
                "\"packet_digest\":\"b23f71d5",
                "\"unknown\":1,\"packet_digest\":\"b23f71d5"
            )
            .as_bytes()
        )
        .is_err()
    );
}

#[test]
fn canonical_packet_uses_json_unicode_escape_policy() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v2.json");
    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.packet_digest.clear();
    packet.required_reads[0].reason = "line\u{2028}separator\u{2029}".to_owned();
    let encoded = canonical_assignment_packet_json_v2(&packet).expect("canonical edge packet");
    assert!(encoded.contains("line\u{2028}separator\u{2029}"));
}

#[test]
fn assignment_packet_rejects_unknown_tools_and_model_flags() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v2.json");
    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.tools.push("actor_invented_tool".to_owned());
    assert!(canonical_assignment_packet_json_v2(&packet).is_err());

    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet
        .model
        .capability_flags
        .push("unrecognized".to_owned());
    assert!(canonical_assignment_packet_json_v2(&packet).is_err());
}

#[test]
fn assignment_packet_seals_inline_policy_source() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v2.json");
    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");

    packet.policy_digest =
        "0000000000000000000000000000000000000000000000000000000000000000".to_owned();
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v2(&packet).is_err());

    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.policy_entrypoint = "main".to_owned();
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v2(&packet).is_err());

    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.policy_byte_limit = 1;
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v2(&packet).is_err());

    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.policy_bytes_b64 = "!!!!".to_owned();
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v2(&packet).is_err());
}

#[test]
fn assignment_packet_uses_rust_integer_widths() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v2.json");
    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.campaign_id = i64::MAX;
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v2(&packet).is_ok());

    packet.campaign_id = -1;
    assert!(canonical_assignment_packet_json_v2(&packet).is_err());

    let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.model.price_input_micro_usd_per_million_tokens = u64::MAX;
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v2(&packet).is_ok());
}

#[test]
fn signed_cas_bytes_have_a_distinct_digest_from_the_unsigned_packet_seal() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v2.json");
    let packet: AssignmentPacketWireV2 = miniserde::json::from_str(raw).expect("fixture DTO");
    let unsigned = unsigned_assignment_packet_digest_v2(&packet).expect("unsigned digest");
    let signed = canonical_assignment_packet_json_v2(&packet).expect("canonical signed bytes");
    assert_ne!(
        factory_protocol::ContentDigest::of_bytes(signed.as_bytes()),
        unsigned
    );
}
