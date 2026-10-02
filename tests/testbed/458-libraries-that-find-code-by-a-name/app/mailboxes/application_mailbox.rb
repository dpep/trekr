class ApplicationMailbox < ActionMailbox::Base
  routing(/^reply/i => :reply)
end
