FROM scratch AS public-trust
COPY keys/cosign.pub keys/tpm2-pcr-signing.public.pem /
