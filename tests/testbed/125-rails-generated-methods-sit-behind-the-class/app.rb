module Statusable
  extend ActiveSupport::Concern

  included do
    enum :status, { active: 0, archived: 1 }
  end
end

class Widget < ActiveRecord::Base
  include Statusable

  def use
    status.upcase
    self.class.statuses
  end
end

class Gadget < ActiveRecord::Base
  def kind
    super || "plain"
  end
  enum :kind, { plain: 0 }

  def use
    kind
  end
end
