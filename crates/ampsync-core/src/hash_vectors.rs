// Test vectors from rclone backend/onedrive/quickxorhash/quickxorhash_test.go (MIT licensed).
// (size, input base64, expected QuickXorHash base64)
pub const VECTORS: &[(usize, &str, &str)] = &[
    (0, "", "AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
    (1, "Sg==", "SgAAAAAAAAAAAAAAAQAAAAAAAAA="),
    (2, "tbQ=", "taAFAAAAAAAAAAAAAgAAAAAAAAA="),
    (3, "0pZP", "0rDEEwAAAAAAAAAAAwAAAAAAAAA="),
    (7, "qaApEj66lw==", "qQBFCiTgA11cAgAABwAAAAAAAAA="),
    (16, "2fuxhBJXtpWFe8dOfdGeHw==", "O9tHLAghgSvYohKFyMMxnNCHaHg="),
    (17, "XBV6YKU9V7yMakZnFIxIkuU=", "HbplHsBQih5cgReMQYMRzkABRiA="),
    (
        32,
        "NbYXsp5/K6mR+NmHwExjvWeWDJFnXTKWVlzYHoesp2E=",
        "wjuAuWDiq04qDt1R8hHWDDcwVoQ=",
    ),
    (
        63,
        "L+LSB8kmGMnHaWVA5P/+qFnfQliXvgJW7d2JGAgT6+koi5NQujFW1bwQVoXrBVyob/gBxGizUoJMgid5gGNo",
        "ndX/KZBtFoeO3xKeo1ajO/Jy+rY=",
    ),
    (
        64,
        "Mb7EGva2rEE5fENDL85P+BsapHEEjv2/siVhKjvAQe02feExVOQSkfmuYzU/kTF1MaKjPmKF/w+cbvwfdWL8aQ==",
        "n1anP5NfvD4XDYWIeRPW3ZkPv1Y=",
    ),
    (
        111,
        "jyibxJSzO6ZiZ0O1qe3tG/bvIAYssvukh9suIT5wEy1JBINVgPiqdsTW0cOpP0aUfP7mgqLfADkzI/m/GgCuVhr8oFLrOCoTx1/psBOWwhltCbhUx51Icm9aH8tY4Z3ccU+6BKpYQkLCy0B/A9Zc",
        "hZfLIilSITC6N3e3tQ/iSgEzkto=",
    ),
    (
        128,
        "ikwCorI7PKWz17EI50jZCGbV9JU2E8bXVfxNMg5zdmqSZ2NlsQPp0kqYIPjzwTg1MBtfWPg53k0h0P2naJNEVgrqpoHTfV2b3pJ4m0zYPTJmUX4Bg/lOxcnCxAYKU29Y5F0U8Quz7ZXFBEweftXxJ7RS4r6N7BzJrPsLhY7hgck=",
        "imAoFvCWlDn4yVw3/oq1PDbbm6U=",
    ),
    (
        222,
        "PfxMcUd0vIW6VbHG/uj/Y0W6qEoKmyBD0nYebEKazKaKG+UaDqBEcmQjbfQeVnVLuodMoPp7P7TR1htX5n2VnkHh22xDyoJ8C/ZQKiSNqQfXvh83judf4RVr9exJCud8Uvgip6aVZTaPrJHVjQhMCp/dEnGvqg0oN5OVkM2qqAXvA0teKUDhgNM71sDBVBCGXxNOR2bpbD1iM4dnuT0ey4L+loXEHTL0fqMeUcEi2asgImnlNakwenDzz0x57aBwyq3AspCFGB1ncX4yYCr/OaCcS5OKi/00WH+wNQU3",
        "QX/YEpG0gDsmhEpCdWhsxDzsfVE=",
    ),
    (
        256,
        "qwGf2ESubE5jOUHHyc94ORczFYYbc2OmEzo+hBIyzJiNwAzC8PvJqtTzwkWkSslgHFGWQZR2BV5+uYTrYT7HVwRM40vqfj0dBgeDENyTenIOL1LHkjtDKoXEnQ0mXAHoJ8PjbNC93zi5TovVRXTNzfGEs5dpWVqxUzb5lc7dwkyvOluBw482mQ4xrzYyIY1t+//OrNi1ObGXuUw2jBQOFfJVj2Y6BOyYmfB1y36eBxi3zxeG5d5NYjm2GSh6e08QMAwu3zrINcqIzLOuNIiGXBtl7DjKt7b5wqi4oFiRpZsCyx2smhSrdrtK/CkdU6nDN+34vSR/M8rZpWQdBE7a8g==",
        "WYT9JY3JIo/pEBp+tIM6Gt2nyTM=",
    ),
    (
        333,
        "w0LGhqU1WXFbdavqDE4kAjEzWLGGzmTNikzqnsiXHx2KRReKVTxkv27u3UcEz9+lbMvYl4xFf2Z4aE1xRBBNd1Ke5C0zToSaYw5o4B/7X99nKK2/XaUX1byLow2aju2XJl2OpKpJg+tSJ2fmjIJTkfuYUz574dFX6/VXxSxwGH/xQEAKS5TCsBK3CwnuG1p5SAsQq3gGVozDWyjEBcWDMdy8/AIFrj/y03Lfc/RNRCQTAfZbnf2QwV7sluw4fH3XJr07UoD0YqN+7XZzidtrwqMY26fpLZnyZjnBEt1FAZWO7RnKG5asg8xRk9YaDdedXdQSJAOy6bWEWlABj+tVAigBxavaluUH8LOj+yfCFldJjNLdi90fVHkUD/m4Mr5OtmupNMXPwuG3EQlqWUVpQoYpUYKLsk7a5Mvg6UFkiH596y5IbJEVCI1Kb3D1",
        "e3+wo77iKcILiZegnzyUNcjCdoQ=",
    ),
];
