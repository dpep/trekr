module Auditable
  extend ActiveSupport::Concern

  class_methods do
    include Reporting

    def audit_log
    end
  end

  def record
    self.class.audit_log
  end
end

module Reporting
  def report
  end
end

class Widget
  include Auditable

  audit_log
  report
end
