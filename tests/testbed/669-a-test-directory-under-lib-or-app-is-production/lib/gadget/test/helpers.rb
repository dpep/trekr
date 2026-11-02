class Gadget
  module Test
    module Helpers
      def use_it(kind)
        Gadget.new.from_lib_test
        Gadget.new.public_send("handle_#{kind}")
        Gadget::BOUNDARY
      end
    end
  end
end
