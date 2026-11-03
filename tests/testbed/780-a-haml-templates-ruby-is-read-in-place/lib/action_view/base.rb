module ActionView
  module Helpers
    module UrlHelper
      def link_to(name, url)
        name
      end
    end
  end

  class Base
    include Helpers::UrlHelper
  end
end
