import re
with open('src/test.rs', 'r') as f:
    content = f.read()
content = content.replace("client.update_heartbeat(&crate::asset_id_to_symbol(&env, asset), &admin);", "client.update_heartbeat(&asset, &admin);")
with open('src/test.rs', 'w') as f:
    f.write(content)
