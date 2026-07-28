import json
from serialization.encoders import compact_json_bytes

# Simulate network submission scenario
def simulate_network_submission(payload):
    """Simulate sending JSON payload over network"""
    # First compact the JSON to remove whitespace and redundant parameters
    compacted_bytes = compact_json_bytes(payload)
    
    # This represents the actual network transmission
    network_payload = compacted_bytes
    
    return network_payload

# Test the network submission flow
test_payload = {
    "status": "success",
    "amount": 100,
    "fee": 0,  # redundant - should be stripped
    "timestamp": "2026-07-27T06:48:28Z",
    "metadata": {
        "user": "test_user",
        "empty_field": "",  # redundant - should be stripped
        "empty_array": [],  # redundant - should be stripped
        "empty_object": {}  # redundant - should be stripped
    },
    "redundant_bool": False,  # redundant - should be stripped
    "redundant_zero": 0,  # redundant - should be stripped
    "valid_data": "important_value"
}

# Process the payload
network_ready_payload = simulate_network_submission(test_payload)

# Verify the result
print(f"Original payload size: {len(json.dumps(test_payload).encode('utf-8'))} bytes")
print(f"Network-ready payload size: {len(network_ready_payload)} bytes")
print(f"Network-ready payload: {network_ready_payload.decode('utf-8')}")

# Basic validation
assert len(network_ready_payload) < len(json.dumps(test_payload).encode('utf-8')), "Payload should be smaller after compaction"
assert b'"fee":0' not in network_ready_payload, "Redundant fee should be removed"
assert b'"empty_field":""' not in network_ready_payload, "Redundant empty field should be removed"
assert b'"empty_array"[]' not in network_ready_payload, "Redundant empty array should be removed"
assert b'"empty_object"{}' not in network_ready_payload, "Redundant empty object should be removed"
assert b'"redundant_bool":false' not in network_ready_payload, "Redundant boolean should be removed"
assert b'"redundant_zero":0' not in network_ready_payload, "Redundant zero should be removed"

print("✅ Network submission simulation passed!")