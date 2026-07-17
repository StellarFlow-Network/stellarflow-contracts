import re

with open('src/test.rs', 'r') as f:
    content = f.read()

# Fix initialize calls
content = content.replace("client.initialize(&admin);", "client.initialize(&admin, &admin);")
content = content.replace("client.try_initialize(&admin);", "client.try_initialize(&admin, &admin);")

# In our tests, we use asset_id_to_symbol to map back to symbol for update_heartbeat.
# But upstream removed it, and they just use asset_sym explicitly.
# We'll just fix our heartbeat tests manually by doing:
# let asset_sym = symbol_short!("KES"); let asset = symbol_to_asset_id(&asset_sym);

with open('src/test.rs', 'w') as f:
    f.write(content)
