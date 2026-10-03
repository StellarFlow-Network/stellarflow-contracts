const fs = require('fs');
const path = require('path');

function walk(dir) {
    let results = [];
    const list = fs.readdirSync(dir);
    list.forEach(function (file) {
        file = path.join(dir, file);
        const stat = fs.statSync(file);
        if (stat && stat.isDirectory()) {
            results = results.concat(walk(file));
        } else {
            if (file.endsWith('.rs')) results.push(file);
        }
    });
    return results;
}

const files = walk('contracts').concat(walk('src'));

for (const file of files) {
    let content = fs.readFileSync(file, 'utf8');
    let modified = false;

    // Convert panics (simple string matches without format args)
    const panicRegex = /panic!\("([^"]+)"\)/g;
    content = content.replace(panicRegex, (match, message) => {
        modified = true;
        let variantName = message.replace(/[^a-zA-Z0-9]/g, ' ')
            .split(' ')
            .filter(w => w.length > 0)
            .map(w => w.charAt(0).toUpperCase() + w.slice(1).toLowerCase())
            .join('');
        if (variantName.length === 0) variantName = "UnknownError";
        return `return Err(ContractError::${variantName})`;
    });

    const specificErrors = [
        'AmmError', 'BuybackError', 'BurnError', 'VestingError', 
        'InvariantError', 'AllowanceError', 'GuardError', 'MedianError',
        'NullifierError'
    ];
    
    for (const name of specificErrors) {
        const regex = new RegExp(`\\b${name}\\b`, 'g');
        if (regex.test(content)) {
            content = content.replace(regex, 'ContractError');
            modified = true;
        }
    }

    // Specially handle `Error` to avoid replacing unrelated `Error` uses (like soroban_sdk::Error or std::error::Error)
    // 1. Result<..., Error>
    const resultRegex = /Result<([^,>]+),\s*Error>/g;
    if (resultRegex.test(content)) {
        content = content.replace(resultRegex, 'Result<$1, ContractError>');
        modified = true;
    }
    // 2. pub enum Error {
    const enumRegex = /pub enum Error\b/g;
    if (enumRegex.test(content)) {
        content = content.replace(enumRegex, 'pub enum ContractError');
        modified = true;
    }
    // 3. Error::Variant (but exclude Error::from or std::error::Error)
    const errorVariantRegex = /\bError::([A-Z][A-Za-z0-9_]*)/g;
    if (errorVariantRegex.test(content)) {
        content = content.replace(errorVariantRegex, (m, v) => {
            return `ContractError::${v}`;
        });
        modified = true;
    }
    // 4. expected = "Error(
    const expectedErrorRegex = /expected\s*=\s*"Error\(/g;
    if (expectedErrorRegex.test(content)) {
        content = content.replace(expectedErrorRegex, 'expected = "ContractError(');
        modified = true;
    }

    // Add recovery steps
    let inEnum = false;
    let lines = content.split('\n');
    let outLines = [];
    
    for (let i = 0; i < lines.length; i++) {
        let line = lines[i];
        if (line.match(/pub enum ContractError/)) {
            inEnum = true;
        } else if (inEnum && line.match(/^\}/)) {
            inEnum = false;
        }
        
        if (inEnum && line.match(/^\s+[A-Z][a-zA-Z0-9_]+\s*=\s*[0-9]+,/)) {
            let prevLine = outLines.length > 0 ? outLines[outLines.length - 1] : '';
            if (!prevLine.includes('Recovery steps:')) {
                let match = line.match(/^\s+([A-Z][a-zA-Z0-9_]+)/);
                if (match) {
                    outLines.push(`    /// Recovery steps: Inspect the state for ${match[1]} and retry with valid inputs or proper conditions.`);
                    modified = true;
                }
            }
        } else if (inEnum && line.match(/^\s+[A-Z][a-zA-Z0-9_]+,/)) {
            let prevLine = outLines.length > 0 ? outLines[outLines.length - 1] : '';
            if (!prevLine.includes('Recovery steps:')) {
                let match = line.match(/^\s+([A-Z][a-zA-Z0-9_]+)/);
                if (match) {
                    outLines.push(`    /// Recovery steps: Inspect the state for ${match[1]} and retry with valid inputs or proper conditions.`);
                    modified = true;
                }
            }
        }
        outLines.push(line);
    }
    
    if (modified) {
        fs.writeFileSync(file, outLines.join('\n'), 'utf8');
    }
}
