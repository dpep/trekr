# A plain module, not ActiveSupport::Concern: its name merely ends in "Concern".
module AuditConcern
  def audit_log(msg)
    puts msg
  end
end
