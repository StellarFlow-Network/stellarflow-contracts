with open('src/lib.rs', 'r') as f:
    text = f.read()

text = text.replace("client.initialize(&admin);", "client.initialize(&admin, &treasury);")
with open('src/lib.rs', 'w') as f:
    f.write(text)
