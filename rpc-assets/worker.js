// Serve apenas os arquivos estáticos do diretório public/.
export default {
  async fetch(request, env) {
    return env.ASSETS.fetch(request);
  },
};
