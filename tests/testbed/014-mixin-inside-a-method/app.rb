module Extra
  def build
  end
end

module Builder
  extend ActiveSupport::Concern

  module ClassMethods
    def build
    end
  end
end

module Installer
  extend ActiveSupport::Concern

  module ClassMethods
    def install
      include Extra
    end
  end
end

class Widget
  include Builder
  include Installer

  build
end
