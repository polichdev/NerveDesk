const modes = {
    control: {
        number: '01 / 03',
        title: 'One cursor.<br>Every screen.',
        description: 'Move your mouse across screen edges and control another computer with the same keyboard and mouse.',
        caption: 'MOVE BETWEEN DEVICES',
    },
    display: {
        number: '02 / 03',
        title: 'A little more<br>room to think.',
        description: 'Mirror your screen or extend your desktop to another device with a virtual display.',
        caption: 'MIRROR OR EXTEND YOUR SCREEN',
    },
    share: {
        number: '03 / 03',
        title: 'Good things<br>travel together.',
        description: 'Keep clipboard content, folders, and audio moving between your connected computers.',
        caption: 'SHARE ACROSS YOUR WORKSPACE',
    },
};

const buttons = document.querySelectorAll('[data-mode]');
buttons.forEach((button) => {
    button.addEventListener('click', () => {
        const mode = modes[button.dataset.mode];
        buttons.forEach((item) => {
            const selected = item === button;
            item.classList.toggle('active', selected);
            item.setAttribute('aria-pressed', String(selected));
        });
        document.querySelector('#demo-number').textContent = mode.number;
        document.querySelector('#demo-title').innerHTML = mode.title;
        document.querySelector('#demo-description').textContent = mode.description;
        document.querySelector('#scene-caption').innerHTML = '<span class="status-dot"></span> ' + mode.caption;
        document.querySelector('.demo-stage').dataset.scene = button.dataset.mode;
    });
});

const positionLabels = {
    left: 'SECOND PC / LEFT ←',
    right: 'SECOND PC / RIGHT →',
    above: 'SECOND PC / ABOVE ↑',
};

const positionButtons = document.querySelectorAll('button[data-position]');
positionButtons.forEach((button) => {
    button.addEventListener('click', () => {
        positionButtons.forEach((item) => {
            const active = item === button;
            item.classList.toggle('active', active);
            item.setAttribute('aria-pressed', String(active));
        });
        document.querySelector('.arrange-board').dataset.position = button.dataset.position;
        document.querySelector('#position-label').textContent = positionLabels[button.dataset.position];
    });
});

const mobileModes = {
    remote: {
        symbol: '↗',
        title: 'Remote control',
        description: 'Your desktop, within reach.',
    },
    camera: {
        symbol: '◉',
        title: 'Continuity camera',
        description: 'Your phone becomes your webcam.',
    },
    share: {
        symbol: '⇧',
        title: 'Quick Share',
        description: 'Send files, photos, text, and links.',
    },
    game: {
        symbol: '◫',
        title: 'Game streaming',
        description: 'Play from your phone browser.',
    },
};

const mobileButtons = document.querySelectorAll('[data-mobile]');
mobileButtons.forEach((button) => {
    button.addEventListener('click', () => {
        const mode = mobileModes[button.dataset.mobile];
        mobileButtons.forEach((item) => {
            const active = item === button;
            item.classList.toggle('active', active);
            item.setAttribute('aria-pressed', String(active));
        });
        document.querySelector('#phone-symbol').textContent = mode.symbol;
        document.querySelector('#phone-title').textContent = mode.title;
        document.querySelector('#phone-description').textContent = mode.description;
        document.querySelector('#phone-preview').dataset.mobileScene = button.dataset.mobile;
    });
});
