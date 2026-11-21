module RuboCop
  module Cop
    module Style
      class NoWidgets < Base
        MSG = "No widgets."
        RESTRICT_ON_SEND = [:widget].freeze

        def on_send(node)
          add_offense(node)
        end
      end
    end
  end
end
