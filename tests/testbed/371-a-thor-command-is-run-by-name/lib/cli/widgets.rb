module Cli
  class Widgets < Thor
    include Pruning

    desc "prune", "Remove widgets nobody uses"
    def prune
      helper_count
    end

    private

    def lonely
      :lonely
    end

    def helper_count
      0
    end
  end
end
