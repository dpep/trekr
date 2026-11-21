module RuboCop
  module Cop
    module Widget
      class NoGadgets < Base
        MSG = "No gadgets."
        RESTRICT_ON_SEND = [:gadget].freeze

        def on_send(node)
          add_offense(node)
        end
      end
    end
  end
end
