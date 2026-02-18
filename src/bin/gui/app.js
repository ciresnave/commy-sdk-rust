// Global state
let currentExample = null;
let runningCount = 0;

// Initialize on page load
document.addEventListener('DOMContentLoaded', () => {
    loadExamples();
    updateRunningCount();
    // Check running examples every 5 seconds
    setInterval(updateRunningCount, 5000);
});

/**
 * Load all examples from API
 */
async function loadExamples() {
    try {
        const response = await fetch('/api/examples');
        const examples = await response.json();
        displayExamples(examples);
    } catch (error) {
        console.error('Failed to load examples:', error);
        showError('Failed to load examples');
    }
}

/**
 * Display examples in the grid
 */
function displayExamples(examples) {
    const grid = document.getElementById('examplesList');
    grid.innerHTML = '';

    examples.forEach((example) => {
        const card = createExampleCard(example);
        grid.appendChild(card);
    });
}

/**
 * Create an example card element
 */
function createExampleCard(example) {
    const card = document.createElement('div');
    card.className = 'example-card';
    card.style.cursor = 'pointer';

    card.innerHTML = `
        <h3>${example.name}</h3>
        <p class="description">${example.description}</p>
        <div class="meta">
            <span>
                <span class="meta-label">Difficulty</span>
                ${example.difficulty}
            </span>
            <span>
                <span class="meta-label">Duration</span>
                ${example.time_estimate}
            </span>
        </div>
    `;

    card.addEventListener('click', () => showExampleDetails(example));

    return card;
}

/**
 * Show example details view
 */
function showExampleDetails(example) {
    currentExample = example;

    document.getElementById('detailName').textContent = example.name;
    document.getElementById('detailDescription').textContent = example.description;
    document.getElementById('detailDifficulty').textContent = example.difficulty;
    document.getElementById('detailTime').textContent = example.time_estimate;

    // Hide run message
    const msg = document.getElementById('runMessage');
    msg.style.display = 'none';
    msg.textContent = '';

    // Switch views
    document.getElementById('mainView').style.display = 'none';
    document.getElementById('detailsView').style.display = 'block';

    // Focus on the view for better UX
    window.scrollTo(0, 0);
}

/**
 * Go back to main examples list
 */
function showMainView() {
    currentExample = null;
    document.getElementById('mainView').style.display = 'block';
    document.getElementById('detailsView').style.display = 'none';
    window.scrollTo(0, 0);
}

/**
 * Run the current example
 */
async function runCurrentExample() {
    if (!currentExample) return;

    try {
        // Show loading state
        const msg = document.getElementById('runMessage');
        msg.style.display = 'block';
        msg.className = 'run-message';
        msg.innerHTML = '⏳ Running example... (this may take a moment)';

        const response = await fetch(`/api/examples/${currentExample.name}/run`, {
            method: 'POST',
        });

        const result = await response.json();

        if (result.success) {
            // Show the output in a formatted way
            msg.className = 'run-message success';
            const output = result.output || 'Example completed successfully';
            msg.innerHTML = `
                <div style="text-align: left;">
                    <strong>✅ ${result.message}</strong>
                    <pre style="background-color: #1e1e1e; color: #d4d4d4; padding: 12px; border-radius: 4px; overflow-x: auto; max-height: 400px; margin-top: 12px; font-size: 12px; line-height: 1.4;">${escapeHtml(output)}</pre>
                </div>
            `;
        } else {
            msg.className = 'run-message error';
            msg.innerHTML = `❌ ${result.message}`;
        }

        updateRunningCount();
    } catch (error) {
        console.error('Failed to run example:', error);
        const msg = document.getElementById('runMessage');
        msg.style.display = 'block';
        msg.className = 'run-message error';
        msg.innerHTML = `❌ Failed to run example: ${error.message}`;
    }
}

/**
 * Escape HTML special characters for safe display
 */
function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

/**
 * Update the count of running examples
 */
async function updateRunningCount() {
    try {
        const response = await fetch('/api/running');
        const running = await response.json();

        let count = 0;
        for (const processes of Object.values(running)) {
            count += processes.length;
        }

        document.getElementById('runningCount').textContent = count;
    } catch (error) {
        console.error('Failed to get running count:', error);
    }
}

/**
 * Show error message
 */
function showError(message) {
    const msg = document.getElementById('runMessage');
    if (msg) {
        msg.style.display = 'block';
        msg.className = 'run-message error';
        msg.textContent = `❌ ${message}`;
    } else {
        alert(message);
    }
}
