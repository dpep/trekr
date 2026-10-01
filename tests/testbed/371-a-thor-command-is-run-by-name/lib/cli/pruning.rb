module Cli
  module Pruning
    extend ActiveSupport::Concern

    included do
      def sweep
        :swept
      end
    end
  end
end
